package httpapi

import (
	"embed"
	"io/fs"
	"net/http"
	"strings"

	"github.com/gin-gonic/gin"

	"github.com/fusedmodel/ncc/ncc-registry/internal/config"
	"github.com/fusedmodel/ncc/ncc-registry/internal/storage"
	"github.com/fusedmodel/ncc/ncc-registry/internal/store"
)

//go:embed web
var consoleFS embed.FS

// NewRouter 组装全部路由。
//
// 路由分四块，对应产品的四条主线：
//
//	/api/auth       账户与凭据
//	/api/registry   制品托管（发布 / 检索 / 下载 / 分发）
//	/api/nodes      托管节点（Agent 发现与互联）
//	/api/cluster    多节点集群（master / worker）与能力路由
func NewRouter(cfg *config.Config, st *store.Store, blob storage.Storage) *gin.Engine {
	s := &Server{Cfg: cfg, St: st, Blob: blob}
	s.hub = newClusterHub(cfg, st)
	s.hub.start()

	r := gin.New()
	r.Use(gin.Logger(), gin.Recovery())
	if cfg.CORSOrigins != "" {
		r.Use(corsMiddleware(cfg.CORSOrigins))
	}

	// 制品字节：公开可读（私有条目走 /api/registry/:ref/bytes 判断可见性）。
	r.Static("/blobs", cfg.BlobDir)

	r.GET("/api/health", func(c *gin.Context) {
		ok(c, 200, gin.H{"ok": true, "service": "ncc-registry", "role": cfg.Role, "nodeId": cfg.NodeID, "time": timeNow()})
	})

	// 节点自述：CLI / 控制台 / 其它节点都靠它认人。
	r.GET("/api/meta", s.meta)

	api := r.Group("/api")
	api.Use(s.authMiddleware())

	api.GET("/auth/meta", s.authMeta)
	api.POST("/auth/register", s.register)
	api.POST("/auth/login", s.login)
	api.GET("/auth/me", requireAuth(), s.me)
	api.PATCH("/auth/me", requireAuth(), s.patchMe)
	api.GET("/auth/keys", requireAuth(), s.listKeys)
	api.GET("/auth/key-scopes", requireAuth(), s.keyScopes)
	api.POST("/auth/keys", requireScope("keys:write"), s.createKey)
	api.DELETE("/auth/keys/:id", requireScope("keys:write"), s.deleteKey)

	api.GET("/namespaces/mine", requireAuth(), s.myNamespaces)
	api.POST("/namespaces", requireAuth(), s.createNamespace)
	api.POST("/namespaces/", requireAuth(), s.createNamespace)
	// 与平台路径一致：设备/节点上报也在这里，且可读。
	api.GET("/namespaces/living", requireAuth(), s.myNodes)
	api.POST("/namespaces/living", requireScope("nodes:write"), s.nodeHeartbeat)

	reg := api.Group("/registry")
	reg.GET("/kinds", s.kinds)
	reg.POST("/uploads", requireScope("registry:publish"), s.upload)
	reg.GET("", s.listRegistry)
	reg.GET("/", s.listRegistry)
	reg.POST("", requireScope("registry:publish"), s.createItem)
	reg.POST("/", requireScope("registry:publish"), s.createItem)
	// 引用有两种形态：A-…（单段）与 @ns/slug（两段）。gin 的 :param 不跨斜杠，
	// 所以两段形式必须单独注册一套（与平台侧同构）。
	reg.GET("/:id/download", s.download)
	reg.GET("/:id/bytes", s.bytes)
	reg.GET("/:id/:slug/download", s.download)
	reg.GET("/:id/:slug/bytes", s.bytes)
	reg.PATCH("/:id", requireScope("registry:publish"), s.patchItem)
	reg.PATCH("/:id/:slug", requireScope("registry:publish"), s.patchItem)
	reg.DELETE("/:id", requireScope("registry:publish"), s.deleteItem)
	reg.DELETE("/:id/:slug", requireScope("registry:publish"), s.deleteItem)
	reg.GET("/:id/:slug", s.getItem)
	reg.GET("/:id", s.getItem)

	nodes := api.Group("/nodes")
	nodes.GET("", s.listNodes)
	nodes.GET("/", s.listNodes)
	nodes.GET("/kinds", s.nodeKinds)
	nodes.GET("/discover", s.discoverNodes)
	nodes.GET("/regions", s.nodeRegions)
	nodes.GET("/route", s.clusterRoute)
	nodes.POST("/heartbeat", requireScope("nodes:write"), s.nodeHeartbeat)
	nodes.POST("/links", requireScope("nodes:write"), s.linkNode)
	nodes.PATCH("/links/:id", requireScope("nodes:write"), s.patchNodeLink)
	nodes.DELETE("/links/:id", requireScope("nodes:write"), s.deleteNodeLink)
	nodes.DELETE("/:id", requireScope("nodes:write"), s.deleteNode)

	// 分发授权：连接解决「找得到」，这里解决「拿得到」。
	api.GET("/grants", requireScope("grants:read"), s.listGrants)
	api.POST("/grants", requireScope("grants:write"), s.createGrant)
	api.DELETE("/grants/:id", requireScope("grants:write"), s.deleteGrant)

	// 接入票据：把「一个内网 registry」加进 Agent —— key/secret 或接入短链。
	acc := api.Group("/access")
	acc.POST("/redeem", s.redeem)
	acc.GET("/tickets", requireAuth(), s.listTickets)
	acc.POST("/tickets", requireScope("keys:write"), s.createTicket)
	acc.GET("/tickets/:key", s.ticketInfo)
	acc.DELETE("/tickets/:id", requireScope("keys:write"), s.deleteTicket)

	// 接入短链落地页（secret 在 URL fragment，服务端看不到）。
	r.GET("/j/:key", s.joinPage)

	cl := api.Group("/cluster")
	cl.POST("/join", s.clusterJoin)
	cl.POST("/heartbeat", s.clusterHeartbeat)
	cl.GET("", s.clusterView)
	cl.GET("/", s.clusterView)
	cl.GET("/workers", s.clusterWorkers)
	cl.GET("/directory", s.clusterDirectory)
	// 集群写：master → worker 分发副本 / 回收副本（节点间，集群 token 鉴权）。
	cl.POST("/ingest", s.clusterIngest)
	cl.POST("/revoke", s.clusterRevoke)
	cl.POST("/replicate", requireScope("registry:publish"), s.replicateArtifact)

	// 内置 Web 控制台（单文件，无构建步骤）。
	if cfg.Console {
		sub, err := fs.Sub(consoleFS, "web")
		if err == nil {
			serveIndex := func(c *gin.Context) {
				// 注意：不能用 c.FileFromFS("index.html", …) —— http.FileServer 见到
				// 以 /index.html 结尾的路径会 301 成 "./"，根路径就被重定向掉了。
				b, err := fs.ReadFile(consoleFS, "web/index.html")
				if err != nil {
					fail(c, 500, "internal", "控制台资源缺失")
					return
				}
				c.Data(http.StatusOK, "text/html; charset=utf-8", b)
			}
			r.GET("/console", func(c *gin.Context) { c.Redirect(http.StatusFound, "/") })
			r.NoRoute(func(c *gin.Context) {
				p := c.Request.URL.Path
				if strings.HasPrefix(p, "/api") || strings.HasPrefix(p, "/blobs") {
					fail(c, 404, "not_found", "未知 API 路径: "+p)
					return
				}
				if c.Request.Method != http.MethodGet && c.Request.Method != http.MethodHead {
					fail(c, 405, "method_not_allowed", "只支持 GET")
					return
				}
				// 静态文件按原路径服务，其余路径一律回控制台首页。
				if fp := strings.TrimPrefix(p, "/"); fp != "" {
					if f, err := sub.Open(fp); err == nil {
						_ = f.Close()
						c.FileFromFS(fp, http.FS(sub))
						return
					}
				}
				serveIndex(c)
			})
		}
	}

	return r
}

// meta GET /api/meta —— 本节点自述（CLI `ncc registry status` 的第一跳）。
func (s *Server) meta(c *gin.Context) {
	artifacts, nodes, users := s.Counts()
	out := gin.H{
		"product": "ncc-registry",
		"about":   "内网托管节点 · 制品托管 · Agent 发现与互联",
		"node":    s.selfNodeJSON(artifacts, nodes, users),
		"counts": gin.H{
			"artifacts": artifacts, "hostedNodes": nodes, "users": users,
		},
		"features": []string{
			"registry: artifact hosting & distribution",
			"nodes: hosted agent/service discovery & linking",
			"grants: explicit access grants (connect != authorize)",
			"access: join by key/secret or one-click intranet link",
			"cluster: master/worker multi-node, routing, replicate & revoke",
		},
		"console": s.Cfg.PublicURL + "/",
		"auth": gin.H{
			"inviteRequired": s.Cfg.InviteRequired(),
			"clusterToken":   s.Cfg.ClusterToken != "",
		},
	}
	if s.Cfg.Role == config.RoleWorker {
		out["masterUrl"] = s.Cfg.MasterURL
	}
	ok(c, 200, out)
}

func corsMiddleware(origins string) gin.HandlerFunc {
	allowed := map[string]bool{}
	star := false
	for _, o := range strings.Split(origins, ",") {
		o = strings.TrimSpace(o)
		if o == "" {
			continue
		}
		if o == "*" {
			star = true
		}
		allowed[o] = true
	}
	return func(c *gin.Context) {
		origin := c.GetHeader("Origin")
		switch {
		case star:
			c.Header("Access-Control-Allow-Origin", "*")
		case origin != "" && allowed[origin]:
			c.Header("Access-Control-Allow-Origin", origin)
			c.Header("Vary", "Origin")
		}
		c.Header("Access-Control-Allow-Headers", "Authorization, Content-Type, X-Filename, X-NCC-Cluster-Token")
		c.Header("Access-Control-Allow-Methods", "GET, POST, PATCH, DELETE, OPTIONS")
		if c.Request.Method == http.MethodOptions {
			c.AbortWithStatus(http.StatusNoContent)
			return
		}
		c.Next()
	}
}
