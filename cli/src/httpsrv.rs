// 最小 HTTP/1.1 服务端（**手写，不引依赖**）。
//
// 为什么自己写：`tiny_http` 那类库对"流式/长连接"的支持会咬人（分块缓冲攒够才发 +
// 只在整体结束时 flush ⇒ 小事件永远发不出去，见本仓库踩坑记录），而这里要的东西很窄：
// 定长 body、我们自己人调用、明确拒绝比半吊子兼容安全。
//
// 两个用户：`gateway.rs`（客户自装的出口代理）与 `app.rs`（本机舱控制台）。
// 协议细节只写一遍 —— 两边各错一种解析才是真的危险。
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

/// 请求头总量上限（防"大头攻击"，也防自己写错循环）。
pub const MAX_HEAD_BYTES: usize = 64 * 1024;

pub struct Req {
    pub method: String,
    /// 原始请求目标，如 `/v1/llm/chat/completions`
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    /// 从 `Authorization: Bearer xxx` 里取令牌。
    pub fn bearer(&self) -> Option<&str> {
        let v = self.header("authorization")?;
        let (kind, tok) = v.split_once(' ')?;
        if kind.eq_ignore_ascii_case("bearer") {
            Some(tok.trim())
        } else {
            None
        }
    }
}

pub enum ReadError {
    /// 请求本身有问题（400/411/413/414 等），带上想回的状态码与原因
    Bad(u16, String),
    Io(std::io::Error),
}

/// 读一个请求：请求行 + 头 + 定长 body。
///
/// 有意**不支持** chunked：我们的调用方是自己人（CLI / Agent），明确拒绝比半吊子解析安全。
pub fn read_request(stream: &mut TcpStream, max_body: u64) -> Result<Req, ReadError> {
    let mut reader = BufReader::new(stream.try_clone().map_err(ReadError::Io)?);

    let mut line = String::new();
    if reader.read_line(&mut line).map_err(ReadError::Io)? == 0 {
        return Err(ReadError::Bad(400, "空请求".into()));
    }
    let mut it = line.trim_end().split(' ');
    let method = it.next().unwrap_or("").to_string();
    let path = it.next().unwrap_or("").to_string();
    let version = it.next().unwrap_or("");
    if method.is_empty() || path.is_empty() || !version.starts_with("HTTP/1.") {
        return Err(ReadError::Bad(400, "请求行不合法".into()));
    }
    if path.len() > 2048 {
        return Err(ReadError::Bad(414, "路径过长".into()));
    }

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut head_bytes = line.len();
    loop {
        let mut hl = String::new();
        let n = reader.read_line(&mut hl).map_err(ReadError::Io)?;
        if n == 0 {
            return Err(ReadError::Bad(400, "请求头未结束".into()));
        }
        head_bytes += n;
        if head_bytes > MAX_HEAD_BYTES {
            return Err(ReadError::Bad(431, "请求头过大".into()));
        }
        let hl = hl.trim_end_matches(['\r', '\n']);
        if hl.is_empty() {
            break;
        }
        let (k, v) = hl
            .split_once(':')
            .ok_or_else(|| ReadError::Bad(400, format!("请求头不合法：{hl}")))?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }

    if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding")) {
        return Err(ReadError::Bad(411, "不支持 chunked 传输（请给 Content-Length）".into()));
    }
    let clen = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let clen: u64 = if clen.is_empty() {
        0
    } else {
        clen.parse().map_err(|_| ReadError::Bad(400, "Content-Length 不是数字".into()))?
    };
    if clen > max_body {
        return Err(ReadError::Bad(413, format!("请求体过大（上限 {max_body} 字节）")));
    }
    let mut body = vec![0u8; clen as usize];
    if clen > 0 {
        reader.read_exact(&mut body).map_err(ReadError::Io)?;
    }
    Ok(Req { method, path, headers, body })
}

pub struct Resp {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub extra: Vec<(String, String)>,
}

impl Resp {
    pub fn json(status: u16, v: &serde_json::Value) -> Self {
        Resp {
            status,
            content_type: "application/json",
            body: v.to_string().into_bytes(),
            extra: Vec::new(),
        }
    }
    pub fn text(status: u16, s: &str) -> Self {
        Resp {
            status,
            content_type: "text/plain; charset=utf-8",
            body: s.as_bytes().to_vec(),
            extra: Vec::new(),
        }
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}

pub fn write_resp(stream: &mut TcpStream, r: &Resp) {
    let mut out = Vec::with_capacity(256 + r.body.len());
    out.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", r.status, reason(r.status)).as_bytes());
    out.extend_from_slice(format!("content-type: {}\r\n", r.content_type).as_bytes());
    out.extend_from_slice(format!("content-length: {}\r\n", r.body.len()).as_bytes());
    out.extend_from_slice(format!("server: ncc/{}\r\n", env!("CARGO_PKG_VERSION")).as_bytes());
    for (k, v) in &r.extra {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(&r.body);
    let _ = stream.write_all(&out);
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}


/// 跑一个服务：每个连接一个线程，`handler` 只管「这个请求回什么」。
///
/// 调用方拿不到 `TcpStream` —— 想直通（转发/流式）的场景不该用这个函数。
pub fn serve<F>(listener: TcpListener, tag: &'static str, max_body: u64, handler: F) -> !
where
    F: Fn(&Req) -> Resp + Send + Sync + 'static,
{
    let handler = std::sync::Arc::new(handler);
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{tag} ⚠ 连接异常：{e}");
                continue;
            }
        };
        let handler = handler.clone();
        std::thread::spawn(move || {
            let mut s = stream;
            match read_request(&mut s, max_body) {
                Ok(req) => write_resp(&mut s, &handler(&req)),
                Err(ReadError::Bad(status, why)) => write_resp(
                    &mut s,
                    &Resp::json(status, &serde_json::json!({ "error": { "code": "bad_request", "message": why } })),
                ),
                Err(ReadError::Io(_)) => {} // 对端断了就断了，不值得刷日志
            }
        });
    }
    unreachable!("incoming() 不会结束")
}
