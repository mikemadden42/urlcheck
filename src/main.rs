use std::error::Error;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::process::ExitCode;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use reqwest::{Client, StatusCode, Url};

const DEFAULT_URL_FILE: &str = "urls.txt";
const HTTP_TIMEOUT_SECONDS: u64 = 15;
const CONNECT_TIMEOUT_SECONDS: u64 = 10;
const MAX_CONCURRENT_CHECKS: usize = 32;
const SEPARATOR: &str = "--------------------------------------------------";

/// Exit code when every URL returned a 2xx status.
const EXIT_ALL_OK: u8 = 0;
/// Exit code when at least one URL failed, errored or was invalid.
const EXIT_SOME_FAILED: u8 = 1;
/// Exit code for usage errors or an unreadable input file.
const EXIT_USAGE: u8 = 2;

/// A non-empty, non-comment line from the input, parsed (or not) as a URL.
#[derive(Debug)]
struct Entry {
    /// The line exactly as written (trimmed), used for display.
    text: String,
    url: Result<Url, String>,
}

/// The result of checking a single URL.
#[derive(Debug)]
enum Outcome {
    /// The server responded. `final_url` differs from the request if redirects were followed.
    Status {
        status: StatusCode,
        final_url: Url,
    },
    Timeout,
    Error(String),
    InvalidUrl(String),
}

impl Outcome {
    fn is_success(&self) -> bool {
        matches!(self, Outcome::Status { status, .. } if status.is_success())
    }
}

/// Parses a URL, accepting only http and https.
fn parse_url(text: &str) -> Result<Url, String> {
    let url = Url::parse(text).map_err(|e| e.to_string())?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!(
            "unsupported scheme '{other}' (expected http or https)"
        )),
    }
}

/// Reads URLs from `reader`, one per line.
///
/// Skips empty lines and lines that are comments (starting with '#').
/// Lines that are not valid http(s) URLs are kept so they can be reported.
fn read_urls(reader: impl BufRead) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();

        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            entries.push(Entry {
                text: trimmed.to_string(),
                url: parse_url(trimmed),
            });
        }
    }
    Ok(entries)
}

/// Formats an error together with its chain of sources, e.g.
/// "error sending request: client error (Connect): dns error: ...".
fn error_chain(e: &dyn Error) -> String {
    let mut msg = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    msg
}

/// Checks a single URL and returns its outcome.
///
/// Sends a HEAD request to avoid downloading the body, falling back to GET
/// for servers that do not support HEAD.
async fn check_url(client: &Client, url: Url) -> Outcome {
    let mut result = client.head(url.clone()).send().await;

    if let Ok(response) = &result {
        if matches!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ) {
            result = client.get(url).send().await;
        }
    }

    match result {
        Ok(response) => Outcome::Status {
            status: response.status(),
            final_url: response.url().clone(),
        },
        Err(e) if e.is_timeout() => Outcome::Timeout,
        Err(e) => Outcome::Error(error_chain(&e)),
    }
}

async fn check_entry(client: &Client, entry: &Entry) -> Outcome {
    match &entry.url {
        Ok(url) => check_url(client, url.clone()).await,
        Err(reason) => Outcome::InvalidUrl(reason.clone()),
    }
}

fn describe(entry: &Entry, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Status { status, final_url } => {
            let redirected = entry.url.as_ref().is_ok_and(|url| url != final_url);
            if redirected {
                format!("STATUS {} (redirected to {final_url})", status.as_u16())
            } else {
                format!("STATUS {}", status.as_u16())
            }
        }
        Outcome::Timeout => format!("Timeout after {HTTP_TIMEOUT_SECONDS}s"),
        Outcome::Error(msg) => format!("HTTP Error: {msg}"),
        Outcome::InvalidUrl(reason) => format!("Invalid URL: {reason}"),
    }
}

fn print_usage() {
    eprintln!("Usage: urlcheck [FILE]");
    eprintln!();
    eprintln!("Checks each URL in FILE (one per line; '#' starts a comment).");
    eprintln!("FILE defaults to '{DEFAULT_URL_FILE}'. Use '-' to read from stdin.");
    eprintln!();
    eprintln!(
        "Exit codes: {EXIT_ALL_OK} = all URLs returned 2xx, {EXIT_SOME_FAILED} = some failed, {EXIT_USAGE} = usage or input error."
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let source = match args.as_slice() {
        [] => DEFAULT_URL_FILE.to_string(),
        [flag] if flag == "-h" || flag == "--help" => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        [path] => path.clone(),
        _ => {
            print_usage();
            return ExitCode::from(EXIT_USAGE);
        }
    };

    // 1. Read URLs
    let read_result = if source == "-" {
        read_urls(io::stdin().lock())
    } else {
        match File::open(&source) {
            Ok(file) => read_urls(BufReader::new(file)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("Error: URL file '{source}' not found.");
                eprintln!(
                    "Create it with one URL per line, or pass a different file as an argument."
                );
                return ExitCode::from(EXIT_USAGE);
            }
            Err(e) => Err(e),
        }
    };
    let source = if source == "-" {
        "stdin".to_string()
    } else {
        source
    };

    let entries = match read_result {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Error reading {source}: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    if entries.is_empty() {
        println!("No URLs found in {source}. Nothing to check.");
        return ExitCode::SUCCESS;
    }

    println!(
        "Found {} URLs in {}. Starting concurrent checks...",
        entries.len(),
        source
    );
    println!("{SEPARATOR}");

    // 2. Setup HTTP Client
    let client = match Client::builder()
        .user_agent("Rust URL Checker (reqwest)")
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECONDS))
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Error creating HTTP client: {}", error_chain(&e));
            return ExitCode::from(EXIT_USAGE);
        }
    };

    // 3. Run checks concurrently (bounded), keeping input order
    let outcomes: Vec<Outcome> = stream::iter(&entries)
        .map(|entry| check_entry(&client, entry))
        .buffered(MAX_CONCURRENT_CHECKS)
        .collect()
        .await;

    // 4. Report results
    let width = entries.iter().map(|e| e.text.len()).max().unwrap_or(0);
    let mut success_count = 0;
    let mut failure_count = 0;

    for (entry, outcome) in entries.iter().zip(&outcomes) {
        if outcome.is_success() {
            success_count += 1;
        } else {
            failure_count += 1;
        }
        println!("{:<width$}  {}", entry.text, describe(entry, outcome));
    }

    println!("{SEPARATOR}");
    println!(
        "Summary: Checked {} URLs ({} successful, {} failed/errored)",
        success_count + failure_count,
        success_count,
        failure_count
    );

    if failure_count > 0 {
        ExitCode::from(EXIT_SOME_FAILED)
    } else {
        ExitCode::from(EXIT_ALL_OK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn read_urls_skips_blanks_and_comments() {
        let input = "\n# a comment\n  https://example.com  \n\n   # indented comment\nhttp://example.org/path\n";
        let entries = read_urls(input.as_bytes()).unwrap();

        let texts: Vec<_> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["https://example.com", "http://example.org/path"]);
        assert!(entries.iter().all(|e| e.url.is_ok()));
    }

    #[test]
    fn read_urls_keeps_invalid_lines() {
        let entries = read_urls("not-a-url\nftp://example.com\n".as_bytes()).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries[0].url.is_err());
        assert!(entries[1].url.as_ref().unwrap_err().contains("ftp"));
    }

    #[test]
    fn only_2xx_is_success() {
        let url = Url::parse("https://example.com").unwrap();
        let status = |code| Outcome::Status {
            status: StatusCode::from_u16(code).unwrap(),
            final_url: url.clone(),
        };

        assert!(status(200).is_success());
        assert!(status(204).is_success());
        assert!(!status(301).is_success());
        assert!(!status(404).is_success());
        assert!(!status(500).is_success());
        assert!(!Outcome::Timeout.is_success());
        assert!(!Outcome::Error("boom".into()).is_success());
        assert!(!Outcome::InvalidUrl("bad".into()).is_success());
    }

    #[test]
    fn describe_reports_redirects() {
        let entry = Entry {
            text: "http://example.com".into(),
            url: parse_url("http://example.com"),
        };
        let same = Outcome::Status {
            status: StatusCode::OK,
            final_url: Url::parse("http://example.com/").unwrap(),
        };
        let moved = Outcome::Status {
            status: StatusCode::OK,
            final_url: Url::parse("https://example.com/").unwrap(),
        };

        assert_eq!(describe(&entry, &same), "STATUS 200");
        assert_eq!(
            describe(&entry, &moved),
            "STATUS 200 (redirected to https://example.com/)"
        );
    }

    /// Serves one canned response per connection: 405 for HEAD, 200 otherwise.
    async fn spawn_head_rejecting_server() -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 1024];
                let n = socket.read(&mut buf).await.unwrap();
                let status = if buf[..n].starts_with(b"HEAD ") {
                    "405 Method Not Allowed"
                } else {
                    "200 OK"
                };
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Url::parse(&format!("http://{addr}/")).unwrap()
    }

    #[tokio::test]
    async fn check_url_falls_back_to_get_when_head_not_allowed() {
        let url = spawn_head_rejecting_server().await;
        let client = Client::new();

        let outcome = check_url(&client, url).await;
        assert!(outcome.is_success(), "unexpected outcome: {outcome:?}");
    }
}
