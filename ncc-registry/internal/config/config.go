// Package config 读取 ncc-registry（内网托管节点）的配置。
//
// 环境变量前缀统一用 NCCR_（NCC Registry node），与平台的 NCC_ 前缀区分开：
// 两者可以同时跑在同一台机器上而互不干扰。所有配置都有可用默认值 ——
// 裸跑 `ncc-registry` 就是一个可用的单节点内网 Registry。
package config

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// 节点角色。
const (
	RoleMaster = "master" // 权威节点：用户/制品/节点目录的唯一权威 + 集群聚合视图
	RoleWorker = "worker" // 边缘节点：自己也托管制品与节点，同时向 master 注册 + 心跳
)

// ValidRole 校验角色取值。
func ValidRole(r string) bool { return r == RoleMaster || r == RoleWorker }

// Config 服务配置。
type Config struct {
	Role      string // master（默认）| worker
	Port      int
	Addr      string
	DataDir   string
	BlobDir   string
	PublicURL string // 别人怎么访问本节点（集群路由与下载 URL 都用它）
	Console   bool   // 是否托管内置 Web 控制台

	NodeID     string // 本节点 id（NCCR_NODE_ID，缺省持久化到 <data>/node-id）
	NodeName   string // 本节点名（NCCR_NODE_NAME，缺省主机名）
	NodeRegion string // 本节点所在区域（NCCR_NODE_REGION，如 上海-内网/机房A）
	NodeRand   string // 生成的短标识，便于同名多机区分

	JWTSecret string
	JWTTTL    time.Duration

	// AccessTTL 接入票据兑换出的节点令牌有效期（默认 30 天；票据带过期时间时取更短的那个）。
	AccessTTL time.Duration

	// 集群：worker 向 master 注册 + 心跳；master 校验 join token。
	MasterURL      string
	ClusterToken   string
	HeartbeatEvery time.Duration
	NodeTTL        time.Duration

	// 注册门禁：留空 = 内网开放注册（默认）；设了值 = 必须带邀请码。
	InviteCode string

	CORSOrigins string
}

func envOr(key, def string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return def
}

func envInt(key string, def int) int {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		if n, err := strconv.Atoi(v); err == nil {
			return n
		}
	}
	return def
}

func envDur(key string, def time.Duration) time.Duration {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		if d, err := time.ParseDuration(v); err == nil {
			return d
		}
		if n, err := strconv.Atoi(v); err == nil { // 纯数字 = 秒
			return time.Duration(n) * time.Second
		}
	}
	return def
}

func envBool(key string, def bool) bool {
	v := strings.ToLower(strings.TrimSpace(os.Getenv(key)))
	switch v {
	case "1", "true", "yes", "on":
		return true
	case "0", "false", "no", "off":
		return false
	}
	return def
}

// Load 读取配置。会确保 DataDir 存在，并落盘 node-id / jwt-secret，
// 这样节点重启后身份与登录态都还在。
func Load() (*Config, error) {
	role := strings.ToLower(envOr("NCCR_ROLE", RoleMaster))
	if !ValidRole(role) {
		return nil, fmt.Errorf("NCCR_ROLE 必须是 master 或 worker，收到 %q", role)
	}
	port := envInt("NCCR_PORT", 8282)
	dataDir := envOr("NCCR_DATA_DIR", "./data")
	if err := os.MkdirAll(dataDir, 0o755); err != nil {
		return nil, fmt.Errorf("创建数据目录失败: %w", err)
	}
	blobDir := envOr("NCCR_BLOB_DIR", filepath.Join(dataDir, "blobs"))
	if err := os.MkdirAll(blobDir, 0o755); err != nil {
		return nil, fmt.Errorf("创建制品目录失败: %w", err)
	}

	name := envOr("NCCR_NODE_NAME", "")
	if name == "" {
		if h, err := os.Hostname(); err == nil && h != "" {
			name = h
		} else {
			name = fmt.Sprintf("ncc-node-%d", port)
		}
	}

	c := &Config{
		Role:      role,
		Port:      port,
		Addr:      fmt.Sprintf(":%d", port),
		DataDir:   dataDir,
		BlobDir:   blobDir,
		PublicURL: strings.TrimRight(envOr("NCCR_PUBLIC_URL", fmt.Sprintf("http://localhost:%d", port)), "/"),
		Console:   envBool("NCCR_CONSOLE", true),

		NodeID:     strings.TrimSpace(os.Getenv("NCCR_NODE_ID")),
		NodeName:   name,
		NodeRegion: envOr("NCCR_NODE_REGION", ""),

		JWTSecret: envOr("NCCR_JWT_SECRET", ""),
		JWTTTL:    envDur("NCCR_JWT_TTL", 168*time.Hour),
		AccessTTL: envDur("NCCR_ACCESS_TTL", 720*time.Hour),

		MasterURL:      strings.TrimRight(envOr("NCCR_MASTER_URL", ""), "/"),
		ClusterToken:   os.Getenv("NCCR_CLUSTER_TOKEN"),
		HeartbeatEvery: envDur("NCCR_HEARTBEAT", 15*time.Second),
		NodeTTL:        envDur("NCCR_NODE_TTL", 60*time.Second),

		InviteCode:  strings.TrimSpace(os.Getenv("NCCR_INVITE_CODE")),
		CORSOrigins: os.Getenv("NCCR_CORS_ORIGINS"),
	}

	// 身份与密钥：缺省落盘，保证重启后不变。
	c.NodeID = persistentSecret(c.NodeID, filepath.Join(dataDir, "node-id"), "ND", 12)
	c.NodeRand = short(c.NodeID)
	c.JWTSecret = persistentSecret(c.JWTSecret, filepath.Join(dataDir, "jwt-secret"), "", 32)

	if c.Role == RoleWorker && c.MasterURL == "" {
		return nil, fmt.Errorf("worker 节点必须配置 NCCR_MASTER_URL（master 地址，如 http://10.0.0.1:8282）")
	}
	return c, nil
}

// InviteRequired 是否需要邀请码。
func (c *Config) InviteRequired() bool { return c.InviteCode != "" }

// InviteAllows 邀请码是否匹配（未启用门禁时一律放行）。
func (c *Config) InviteAllows(code string) bool {
	if !c.InviteRequired() {
		return true
	}
	for _, want := range strings.Split(c.InviteCode, ",") {
		if strings.TrimSpace(want) == strings.TrimSpace(code) {
			return true
		}
	}
	return false
}

// persistentSecret 沿用传入值；为空则读文件；文件也没有就生成并写入。
func persistentSecret(cur, path, prefix string, n int) string {
	if cur != "" {
		return cur
	}
	if b, err := os.ReadFile(path); err == nil {
		if v := strings.TrimSpace(string(b)); v != "" {
			return v
		}
	}
	buf := make([]byte, n)
	_, _ = rand.Read(buf)
	v := hex.EncodeToString(buf)
	if prefix != "" {
		v = prefix + "-" + v
	}
	_ = os.WriteFile(path, []byte(v), 0o600)
	return v
}

func short(id string) string {
	if len(id) <= 8 {
		return id
	}
	return id[len(id)-8:]
}
