use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

use futures::future;
use reqwest::Client;
use tokio::time::timeout;

// Define the name of the file containing the list of URLs
const URL_FILE_NAME: &str = "urls.txt";
const HTTP_TIMEOUT_SECONDS: u64 = 15;

/// Checks a single URL asynchronously and returns its status.
///
/// Handles various errors (timeout, connection failure, DNS issues)
/// and extracts the HTTP status code for successful responses.
async fn check_url(client: &Client, url: String) -> (String, String) {
    let result = timeout(
        Duration::from_secs(HTTP_TIMEOUT_SECONDS),
        client.get(&url).send(),
    )
    .await;

    let status_str = match result {
        Ok(Ok(response)) => format!("STATUS {}", response.status().as_u16()),
        Ok(Err(e)) => format!("HTTP Error: {e}"),
        Err(_) => format!("Timeout after {HTTP_TIMEOUT_SECONDS}s"),
    };

    (url, status_str)
}

/// Reads the list of URLs from a specified file path.
///
/// Skips empty lines and lines that are likely comments (starting with '#').
fn read_urls_from_file(path: &Path) -> io::Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut urls = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();

        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            urls.push(trimmed.to_string());
        }
    }
    Ok(urls)
}

/// Main entry point for the URL checking logic.
///
/// This function is marked with `#[tokio::main]` to set up the asynchronous runtime.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(URL_FILE_NAME);

    if !path.exists() {
        eprintln!("Error: URL file not found.");
        eprintln!(
            "Please create a file named '{URL_FILE_NAME}' in the current directory and list URLs, one per line."
        );
        return Err(Box::from(format!("{URL_FILE_NAME} not found")));
    }

    // 1. Read URLs
    let urls = match read_urls_from_file(path) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Error reading {URL_FILE_NAME}: {e}");
            return Err(e.into());
        }
    };

    if urls.is_empty() {
        println!("No URLs found in {URL_FILE_NAME}. Nothing to check.");
        return Ok(());
    }

    println!(
        "Found {} URLs in {}. Starting concurrent checks...",
        urls.len(),
        URL_FILE_NAME
    );
    println!("--------------------------------------------------");

    // 2. Setup HTTP Client
    let client = Client::builder()
        .user_agent("Rust URL Checker (reqwest)")
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    // 3. Create a vector of futures (async tasks)
    let futures: Vec<_> = urls
        .into_iter()
        .map(|url| check_url(&client, url))
        .collect();

    // 4. Run all checks concurrently
    let results = future::join_all(futures).await;

    // 5. Report results
    let mut success_count = 0;
    let mut failure_count = 0;

    for (url, status) in results {
        let status_code = status.split(' ').nth(1).unwrap_or("ERROR");
        if status_code.starts_with('2') {
            success_count += 1;
        } else {
            failure_count += 1;
        }
        println!("{url:<70} {status}");
    }

    println!("--------------------------------------------------");
    println!(
        "Summary: Checked {} URLs ({} successful, {} failed/errored)",
        success_count + failure_count,
        success_count,
        failure_count
    );

    Ok(())
}
