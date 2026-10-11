//! A small HTTP/1.1 client for web services and downloads, on std only.
//!
//! [`get`], [`post`] and [`request`] block until the whole response is in;
//! [`download`] streams a body into a file. From game code, use [`fetch`],
//! which runs the request on its own thread, and poll the returned
//! [`PendingHttp`] once a frame so the frame never waits on the network.
//!
//! ```no_run
//! use rusting_engine::net::http;
//! let scores = http::get("http://scores.example.com/top?game=swarm")?;
//! if scores.status == 200 {
//!     println!("{}", scores.text());
//! }
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Only `http://` URLs work: there is no TLS, so an `https://` URL fails
//! with [`io::ErrorKind::Unsupported`]. Redirects to another `http://` URL
//! are followed, up to [`MAX_REDIRECTS`]; headers you pass go only to the
//! first URL's host, not to a redirect that leaves it.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Duration;

/// Redirects followed before a request fails.
pub const MAX_REDIRECTS: usize = 5;
/// Largest body [`request`] keeps in memory; [`download`] has no limit.
pub const MAX_BODY: u64 = 64 << 20;
/// Longest the status line and headers may be.
const MAX_HEAD: u64 = 64 << 10;
/// Longest a connect, or a wait for the next bytes, may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A whole response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names in lower case, in the order they came.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The first header called `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// The body as text, with invalid UTF-8 replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// `GET url`.
pub fn get(url: &str) -> io::Result<HttpResponse> {
    request("GET", url, &[], &[])
}

/// `POST url` with `body` of type `content_type`, such as
/// `application/json`.
pub fn post(
    url: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<HttpResponse> {
    request("POST", url, &[("Content-Type", content_type)], body)
}

/// Sends `method url` with extra `headers` and `body` and reads the whole
/// response. Any status is a response; check `status` yourself. Fails
/// when the body is over [`MAX_BODY`].
pub fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<HttpResponse> {
    let mut kept = Vec::new();
    let (status, headers) =
        send(method, url, headers, body, &mut kept, Some(MAX_BODY))?;
    Ok(HttpResponse {
        status,
        headers,
        body: kept,
    })
}

/// Downloads `url` into the file `to` and returns its size in bytes. The
/// body goes to `<to>.part` first and replaces `to` only once it is
/// whole, so a failed download never leaves a cut-off file at `to`. Any
/// status but 2xx fails.
pub fn download(url: &str, to: &Path) -> io::Result<u64> {
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    let part = Path::new(&part);
    let result = (|| {
        let mut file = io::BufWriter::new(std::fs::File::create(part)?);
        let (status, _) = send("GET", url, &[], &[], &mut file, None)?;
        if !(200..300).contains(&status) {
            return Err(io::Error::other(format!("{url}: HTTP {status}")));
        }
        file.flush()?;
        drop(file);
        std::fs::rename(part, to)?;
        std::fs::metadata(to).map(|meta| meta.len())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(part);
    }
    result
}

/// A [`request`] running on its own thread; see [`fetch`].
pub struct PendingHttp(Option<Receiver<io::Result<HttpResponse>>>);

impl PendingHttp {
    /// The response once it is in, `None` while it is still on its way.
    /// It returns `Some` once, then `None` again.
    pub fn poll(&mut self) -> Option<io::Result<HttpResponse>> {
        let result = match self.0.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err(io::Error::other("the request thread stopped"))
            }
        };
        self.0 = None;
        Some(result)
    }
}

/// Starts [`request`] on a new thread, so a game can poll for the answer
/// once a frame instead of waiting on it.
pub fn fetch(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> PendingHttp {
    let (sender, receiver) = channel();
    let (method, url, body) =
        (method.to_owned(), url.to_owned(), body.to_vec());
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    std::thread::spawn(move || {
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let _ = sender.send(request(&method, &url, &headers, &body));
    });
    PendingHttp(Some(receiver))
}

fn find_header<'a>(
    headers: &'a [(String, String)],
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// The `host:port` to connect to, the `Host` header, and the path.
struct Url {
    address: String,
    host: String,
    path: String,
}

fn parse_url(url: &str) -> io::Result<Url> {
    // A space or line break would end the request line early and let the
    // rest of the URL (or a redirect's Location) write its own headers.
    if url.contains(|c: char| c.is_control() || c.is_whitespace()) {
        return Err(invalid(format!(
            "{url:?}: spaces and control characters must be %-encoded"
        )));
    }
    let rest = match url.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("https") => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{url}: HTTPS needs TLS, which is not built in; use http://"),
            ));
        }
        _ => return Err(invalid(format!("{url}: not an http:// URL"))),
    };
    let at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, tail) = rest.split_at(at);
    let tail = tail.split('#').next().unwrap_or("");
    let path = if tail.starts_with('/') {
        tail.to_owned()
    } else {
        format!("/{tail}")
    };
    if host.is_empty() || host.contains('@') {
        return Err(invalid(format!("{url}: no host, or a user name in it")));
    }
    // A port follows the last colon, unless that colon is inside an IPv6
    // literal such as [::1].
    let has_port = host.rfind(':').is_some_and(|at| !host[at..].contains(']'));
    let address = if has_port {
        host.to_owned()
    } else {
        format!("{host}:80")
    };
    Ok(Url {
        address,
        host: host.to_owned(),
        path,
    })
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn bad_response(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// Sends the request, following redirects, and writes the final body to
/// `sink`; returns its status and headers.
fn send(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    sink: &mut dyn Write,
    limit: Option<u64>,
) -> io::Result<(u16, Vec<(String, String)>)> {
    let token = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    };
    if !token(method) {
        return Err(invalid(format!("`{method}` is not an HTTP method")));
    }
    for (name, value) in headers {
        if !token(name) || value.contains(['\r', '\n']) {
            return Err(invalid(format!(
                "header `{name}` is not a name or has a line break"
            )));
        }
    }
    let first_host = parse_url(url)?.host;
    let (mut method, mut url, mut body) =
        (method.to_owned(), url.to_owned(), body);
    // The caller's headers (an Authorization token, say) go only to the
    // host they were meant for, never on to wherever a redirect points.
    let mut same_host = true;
    for _ in 0..=MAX_REDIRECTS {
        let target = parse_url(&url)?;
        same_host &= target.host.eq_ignore_ascii_case(&first_host);
        let mut stream = connect(&target.address)?;
        let mut head = format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: rusting-engine\r\nConnection: close\r\n",
            target.path, target.host
        );
        for (name, value) in headers.iter().filter(|_| same_host) {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if !body.is_empty()
            || matches!(method.as_str(), "POST" | "PUT" | "PATCH")
        {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let (status, response_headers) = read_head(&mut reader)?;
        let location = find_header(&response_headers, "location");
        if let (301 | 302 | 303 | 307 | 308, Some(location)) =
            (status, location)
        {
            url = if location.starts_with('/') {
                format!("http://{}{location}", target.host)
            } else {
                location.to_owned()
            };
            // Like browsers, only 307 and 308 send the body again.
            if !matches!(status, 307 | 308) {
                method = "GET".to_owned();
                body = &[];
            }
            continue;
        }
        let empty = method == "HEAD" || matches!(status, 100..=199 | 204 | 304);
        if !empty {
            read_body(&mut reader, &response_headers, sink, limit)?;
        }
        return Ok((status, response_headers));
    }
    Err(bad_response("too many redirects"))
}

fn connect(address: &str) -> io::Result<TcpStream> {
    let mut last = io::Error::new(
        io::ErrorKind::NotFound,
        format!("{address}: no address"),
    );
    for socket in address.to_socket_addrs()? {
        match TcpStream::connect_timeout(&socket, TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(TIMEOUT))?;
                stream.set_write_timeout(Some(TIMEOUT))?;
                return Ok(stream);
            }
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Reads one CRLF line of the head, counting it against `left`.
fn head_line(reader: &mut impl BufRead, left: &mut u64) -> io::Result<String> {
    let mut line = Vec::new();
    reader.by_ref().take(*left).read_until(b'\n', &mut line)?;
    if !line.ends_with(b"\n") {
        return Err(bad_response("the response head is cut off or too long"));
    }
    *left -= line.len() as u64;
    let line = String::from_utf8_lossy(&line);
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn read_head(
    reader: &mut impl BufRead,
) -> io::Result<(u16, Vec<(String, String)>)> {
    let mut left = MAX_HEAD;
    let status_line = head_line(reader, &mut left)?;
    let status = status_line
        .strip_prefix("HTTP/1.")
        .and_then(|rest| rest.get(2..5))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| bad_response("not an HTTP/1.x status line"))?;
    let mut headers = Vec::new();
    loop {
        let line = head_line(reader, &mut left)?;
        if line.is_empty() {
            return Ok((status, headers));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| bad_response("a header without a colon"))?;
        headers
            .push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
}

fn read_body(
    reader: &mut impl BufRead,
    headers: &[(String, String)],
    sink: &mut dyn Write,
    limit: Option<u64>,
) -> io::Result<()> {
    let mut written = 0u64;
    let mut copy =
        |part: &mut dyn Read, sink: &mut dyn Write| -> io::Result<u64> {
            let room = limit.map_or(u64::MAX, |limit| limit - written + 1);
            let copied = io::copy(&mut part.take(room), sink)?;
            written += copied;
            if limit.is_some_and(|limit| written > limit) {
                return Err(bad_response("the body is over the size limit"));
            }
            Ok(copied)
        };
    let chunked = find_header(headers, "transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    if chunked {
        loop {
            let mut left = MAX_HEAD;
            let line = head_line(reader, &mut left)?;
            let size = line.split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size, 16)
                .map_err(|_| bad_response("a bad chunk size"))?;
            if size == 0 {
                // Trailers, up to the empty line.
                while !head_line(reader, &mut left)?.is_empty() {}
                return Ok(());
            }
            if copy(&mut reader.by_ref().take(size), sink)? != size {
                return Err(bad_response("the body is cut off"));
            }
            if !head_line(reader, &mut left)?.is_empty() {
                return Err(bad_response("a chunk runs past its size"));
            }
        }
    }
    match find_header(headers, "content-length") {
        Some(length) => {
            let length: u64 = length
                .parse()
                .map_err(|_| bad_response("a bad Content-Length"))?;
            if copy(&mut reader.by_ref().take(length), sink)? != length {
                return Err(bad_response("the body is cut off"));
            }
        }
        // Without a length the body ends when the server closes.
        None => {
            copy(reader, sink)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves each of `replies` to one connection in turn and returns the
    /// server's address and the requests it read.
    fn serve(
        replies: Vec<Vec<u8>>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(value) = line.strip_prefix("Content-Length: ") {
                        length = value.trim().parse().unwrap();
                    }
                    request.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                request.push_str(&String::from_utf8(body).unwrap());
                requests.push(request);
                stream.write_all(&reply).unwrap();
            }
            requests
        });
        (address, server)
    }

    #[test]
    fn requests_read_lengths_chunks_and_closes_and_follow_redirects() {
        let (address, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Score: 9\r\n\r\nhello".to_vec(),
            b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n3;x=1\r\nabc\r\n2\r\nde\r\n0\r\nT: 1\r\n\r\n".to_vec(),
            b"HTTP/1.1 303 See Other\r\nLocation: /next\r\n\r\n".to_vec(),
            b"HTTP/1.0 404 Not Found\r\n\r\ngone until close".to_vec(),
        ]);
        let url = format!("http://{address}");

        let first = get(&format!("{url}/a?b=c#frag")).unwrap();
        assert_eq!((first.status, first.text().as_str()), (200, "hello"));
        assert_eq!(first.header("X-SCORE"), Some("9"));

        let second = post(&url, "application/json", b"{\"n\":1}").unwrap();
        assert_eq!(
            (second.status, second.body.as_slice()),
            (201, &b"abcde"[..])
        );

        // The 303 turns the POST into a GET of /next, which has no length.
        let third = post(&format!("{url}/form"), "text/plain", b"x").unwrap();
        assert_eq!(
            (third.status, third.text().as_str()),
            (404, "gone until close")
        );

        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /a?b=c HTTP/1.1\r\n"));
        assert!(requests[0].contains(&format!("Host: {address}\r\n")));
        assert!(requests[1].starts_with("POST / HTTP/1.1\r\n"));
        assert!(requests[1].contains("Content-Type: application/json\r\n"));
        assert!(requests[1].ends_with("\r\n\r\n{\"n\":1}"));
        assert!(requests[2].starts_with("POST /form HTTP/1.1\r\n"));
        assert!(requests[3].starts_with("GET /next HTTP/1.1\r\n"));
        assert!(!requests[3].contains("Content-Length"));
    }

    #[test]
    fn caller_headers_stay_with_the_first_host_across_redirects() {
        let (elsewhere, other) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ]);
        let (address, first) = serve(vec![
            b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /again\r\n\r\n".to_vec(),
            format!("HTTP/1.1 308 Permanent Redirect\r\nLocation: http://{elsewhere}/x\r\n\r\n")
                .into_bytes(),
        ]);
        let secret = [("Authorization", "Bearer secret")];
        let response =
            request("PUT", &format!("http://{address}/"), &secret, b"data")
                .unwrap();
        assert_eq!(response.status, 200);

        let first = first.join().unwrap();
        assert!(first
            .iter()
            .all(|r| r.contains("Authorization: Bearer secret\r\n")));
        let other = other.join().unwrap();
        assert!(other[0].starts_with("PUT /x HTTP/1.1\r\n"), "{}", other[0]);
        assert!(!other[0].contains("secret"), "{}", other[0]);
        assert!(other[0].ends_with("\r\n\r\ndata"), "a 308 resends the body");
    }

    #[test]
    fn downloads_land_whole_or_not_at_all_and_fetch_runs_off_thread() {
        let (address, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nPNG!".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\ncut".to_vec(),
            b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        ]);
        let folder = std::env::temp_dir()
            .join(format!("rusting-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("icon.png");
        let url = format!("http://{address}/icon.png");

        assert_eq!(download(&url, &file).unwrap(), 4);
        assert_eq!(std::fs::read(&file).unwrap(), b"PNG!");
        let cut = download(&url, &folder.join("cut.png")).unwrap_err();
        assert_eq!(cut.kind(), io::ErrorKind::InvalidData);
        assert!(download(&url, &folder.join("err.png")).is_err());
        let left: Vec<_> = std::fs::read_dir(&folder).unwrap().collect();
        assert_eq!(left.len(), 1, "only the whole download stays");

        let mut pending = fetch("GET", &url, &[("Accept", "text/plain")], &[]);
        let response = loop {
            if let Some(result) = pending.poll() {
                break result.unwrap();
            }
            std::thread::yield_now();
        };
        assert_eq!(response.text(), "ok");
        assert!(pending.poll().is_none(), "the response comes once");
        assert!(server.join().unwrap()[3].contains("Accept: text/plain\r\n"));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn urls_and_headers_that_cannot_be_sent_fail_before_connecting() {
        let https = get("https://example.com/").unwrap_err();
        assert_eq!(https.kind(), io::ErrorKind::Unsupported);
        for bad in [
            "ftp://example.com/",
            "example.com",
            "http:///x",
            "http://me@example.com/",
        ] {
            assert_eq!(
                get(bad).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{bad}"
            );
        }
        let injected = request(
            "GET",
            "http://127.0.0.1:9/",
            &[("X", "a\r\nEvil: 1")],
            &[],
        );
        assert_eq!(injected.unwrap_err().kind(), io::ErrorKind::InvalidInput);

        for bad in ["http://h/a b", "http://h/a\r\nEvil: 1", "http://h/\u{7f}"]
        {
            assert_eq!(
                get(bad).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{bad:?}"
            );
        }
        for method in ["", "GET /x HTTP/1.1\r\n", "G T"] {
            let error =
                request(method, "http://127.0.0.1:9/", &[], &[]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{method:?}");
        }
        let colon =
            request("GET", "http://127.0.0.1:9/", &[("X: y\r\nZ", "1")], &[]);
        assert_eq!(colon.unwrap_err().kind(), io::ErrorKind::InvalidInput);

        let url = parse_url("http://[::1]/p").unwrap();
        assert_eq!(
            (url.address.as_str(), url.path.as_str()),
            ("[::1]:80", "/p")
        );
        let url = parse_url("http://[::1]:8080?q=1").unwrap();
        assert_eq!(
            (url.address.as_str(), url.path.as_str()),
            ("[::1]:8080", "/?q=1")
        );
    }
}
