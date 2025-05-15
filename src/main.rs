use std::collections::VecDeque;
use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// 3.1 WebsiteStatus Structure
#[derive(Debug, Clone)]
struct WebsiteStatus {
    url: String,
    action_status: Result<u16, String>, // HTTP code or error text
    response_time: Duration,            // how long the request took
    timestamp: SystemTime,              // when the attempt completed
}

// Struct to hold configuration
#[derive(Debug, Clone)]
struct Config {
    timeout: Duration,
    retries: u32,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();

    let mut urls_to_check: Vec<String> = Vec::new();
    let mut file_path: Option<String> = None;
    let mut num_workers: usize = std::thread::available_parallelism().map_or(2, |nz| nz.get());
    let mut timeout_seconds: u64 = 5;
    let mut retries_count: u32 = 0;

    // --- Argument Parsing ---
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                i += 1;
                if i < args.len() {
                    file_path = Some(args[i].clone());
                } else {
                    return Err("--file requires an argument".to_string());
                }
            }
            "--workers" => {
                i += 1;
                if i < args.len() {
                    num_workers = args[i]
                        .parse()
                        .map_err(|_| format!("Invalid number for --workers: {}", args[i]))?;
                    if num_workers == 0 {
                        return Err("--workers must be at least 1".to_string());
                    }
                } else {
                    return Err("--workers requires an argument".to_string());
                }
            }
            "--timeout" => {
                i += 1;
                if i < args.len() {
                    timeout_seconds = args[i]
                        .parse()
                        .map_err(|_| format!("Invalid number for --timeout: {}", args[i]))?;
                    if timeout_seconds == 0 {
                        return Err("--timeout must be at least 1 second".to_string());
                    }
                } else {
                    return Err("--timeout requires an argument".to_string());
                }
            }
            "--retries" => {
                i += 1;
                if i < args.len() {
                    retries_count = args[i]
                        .parse()
                        .map_err(|_| format!("Invalid number for --retries: {}", args[i]))?;
                } else {
                    return Err("--retries requires an argument".to_string());
                }
            }
            "-h" | "--help" => {
                print_usage(&args[0]);
                return Ok(());
            }
            arg_str if arg_str.starts_with("--") => {
                return Err(format!("Unknown option: {}", arg_str));
            }
            arg_str => {
                urls_to_check.push(arg_str.to_string());
            }
        }
        i += 1;
    }

    // --- Load URLs from file ---
    if let Some(path_str) = &file_path {
        let path = Path::new(path_str.as_str());
        let file = File::open(path)
            .map_err(|e| format!("Failed to open file {}: {}", path_str, e))?;
        let reader = io::BufReader::new(file);
        for line_result in reader.lines() {
            let line = line_result.map_err(|e| format!("Failed to read line from file: {}", e))?;

            // --- FIX for comments on lines ---
            // Find the position of '#' if it exists
            let line_without_comment = if let Some(comment_start) = line.find('#') {
                if comment_start == 0 { // Line starts with #, it's a full comment line
                    "" // Treat as empty to be skipped
                } else {
                    &line[..comment_start] // Take the slice before the '#'
                }
            } else {
                &line // No comment, take the whole line
            };
            let trimmed_url_part = line_without_comment.trim(); // Trim whitespace
            // --- END FIX ---

            // Now, trimmed_url_part should only be the URL or empty.
            // The original spec said "lines starting with # are ignored", which this handles.
            // And "blank lines are ignored".
            if !trimmed_url_part.is_empty() {
                urls_to_check.push(trimmed_url_part.to_string());
            }
        }
    }

    // --- Validate URL input ---
    if urls_to_check.is_empty() {
        print_usage(&args[0]);
        eprintln!("\nError: No URLs provided via --file or positional arguments.");
        std::process::exit(2);
    }

    // Deduplicate URLs
    let mut seen_urls = std::collections::HashSet::new();
    urls_to_check.retain(|url| seen_urls.insert(url.clone()));


    let config = Arc::new(Config {
        timeout: Duration::from_secs(timeout_seconds),
        retries: retries_count,
    });

    let jobs_queue = Arc::new(Mutex::new(VecDeque::from(urls_to_check.clone())));
    let num_total_jobs = urls_to_check.len();

    let (result_tx, result_rx): (Sender<WebsiteStatus>, Receiver<WebsiteStatus>) = channel();

    let client = Arc::new(
        reqwest::blocking::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?,
    );

    // --- Worker Threads ---
    let mut worker_handles = Vec::new();
    for worker_id in 0..num_workers {
        let jobs_queue_clone = Arc::clone(&jobs_queue);
        let result_tx_clone = result_tx.clone();
        let client_clone = Arc::clone(&client);
        let config_clone = Arc::clone(&config);

        let handle = thread::spawn(move || {
            loop {
                let url_to_check: String = match jobs_queue_clone.lock() {
                    Ok(mut queue_guard) => {
                        if let Some(url) = queue_guard.pop_front() {
                            url
                        } else {
                            break;
                        }
                    }
                    Err(poisoned) => {
                        eprintln!("Worker {}: Job queue mutex poisoned: {}. Shutting down worker.", worker_id, poisoned);
                        break;
                    }
                };

                let mut final_status_result: Option<WebsiteStatus> = None;

                for attempt in 0..=(config_clone.retries) {
                    let start_time = Instant::now();
                    let request_result = client_clone.get(&url_to_check).send();
                    let response_time = start_time.elapsed();
                    let timestamp = SystemTime::now();

                    match request_result {
                        Ok(response) => {
                            final_status_result = Some(WebsiteStatus {
                                url: url_to_check.clone(),
                                action_status: Ok(response.status().as_u16()),
                                response_time,
                                timestamp,
                            });
                            break;
                        }
                        Err(e) => {
                            if attempt >= config_clone.retries {
                                final_status_result = Some(WebsiteStatus {
                                    url: url_to_check.clone(),
                                    action_status: Err(e.to_string()),
                                    response_time,
                                    timestamp,
                                });
                                break;
                            }
                            if attempt < config_clone.retries {
                                thread::sleep(Duration::from_millis(100));
                            }
                        }
                    }
                }

                if let Some(status) = final_status_result {
                    if result_tx_clone.send(status).is_err() {
                        break;
                    }
                }
            }
        });
        worker_handles.push(handle);
    }

    drop(result_tx);

    // --- Collect Results and Live Output ---
    let mut all_statuses: Vec<WebsiteStatus> = Vec::with_capacity(num_total_jobs);
    println!(
        "{:<30} | {:<8} | {:<12} | {}",
        "URL", "Status", "Time (ms)", "Timestamp (EpochS)"
    );
    println!("{}", "-".repeat(75));

    for _ in 0..num_total_jobs {
        match result_rx.recv() {
            Ok(status) => {
                let status_str = match &status.action_status {
                    Ok(code) => code.to_string(),
                    Err(e_str) => {
                        if e_str.len() > 20 {
                            format!("ERR: {}...", &e_str[..17])
                        } else {
                            format!("ERR: {}", e_str)
                        }
                    }
                };
                let time_ms = status.response_time.as_millis();
                let timestamp_epoch_s = status.timestamp
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                println!(
                    "{:<30} | {:<8} | {:<12} | {}",
                    truncate_url(&status.url, 28),
                    status_str,
                    time_ms,
                    timestamp_epoch_s
                );
                all_statuses.push(status);
            }
            Err(_) => {
                break;
            }
        }
    }

    // --- Wait for all worker threads to finish ---
    for (i,handle) in worker_handles.into_iter().enumerate() {
        if handle.join().is_err() {
            eprintln!("Error: Worker thread {} panicked.", i);
        }
    }

    // --- Batch Output (JSON) ---
    if !all_statuses.is_empty() {
        write_json_output(&all_statuses, "status.json")?;
        println!("\nResults written to status.json");
    } else if num_total_jobs > 0 {
        println!("\nNo results were successfully processed to write to status.json.");
    }

    Ok(())
}

fn print_usage(program_name: &str) {
    eprintln!("Website Status Checker");
    eprintln!("\nUsage: {} [OPTIONS] [URL...]", program_name);
    eprintln!("\nChecks the availability of websites concurrently.");
    eprintln!("\nOptions:");
    eprintln!("  --file <path>        Path to a text file containing URLs (one per line).");
    eprintln!("                       Lines starting with # and blank lines are ignored.");
    eprintln!("  --workers <N>        Number of worker threads (default: number of logical CPU cores, min 1).");
    eprintln!("  --timeout <seconds>  Per-request timeout in seconds (default: 5, min 1).");
    eprintln!("  --retries <N>        Number of additional attempts after a failure (default: 0).");
    eprintln!("  -h, --help           Show this help message and exit.");
    eprintln!("\nIf neither --file nor positional URLs are supplied, this message is shown and the program exits with code 2.");
    eprintln!("\nJSON Output Fields (in status.json):");
    eprintln!("  url (String):             The original URL checked.");
    eprintln!("  status (Number or String): HTTP status code (e.g., 200) if successful, or an error message string if failed.");
    eprintln!("  responseTimeMs (Number):  Total response time in milliseconds for the final attempt.");
    eprintln!("  timestampEpochS (Number): Timestamp of when the attempt completed, as seconds since UNIX_EPOCH.");
}

fn truncate_url(url: &str, max_len: usize) -> String {
    if url.len() > max_len && max_len > 3 {
        format!("{}...", &url[..max_len - 3])
    } else {
        url.to_string()
    }
}

fn escape_json_string(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len() + 10);
    for c in s.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(c),
        }
    }
    escaped
}

fn write_json_output(statuses: &[WebsiteStatus], file_path: &str) -> Result<(), String> {
    let file = File::create(file_path)
        .map_err(|e| format!("Failed to create JSON output file {}: {}", file_path, e))?;
    let mut writer = BufWriter::new(file);

    writer.write_all(b"[\n").map_err(|e| format!("JSON write error: {}", e))?;

    for (i, status) in statuses.iter().enumerate() {
        writer.write_all(b"  {\n").map_err(|e| format!("JSON write error: {}", e))?;

        let url_json = format!("    \"url\": \"{}\",\n", escape_json_string(&status.url));
        writer.write_all(url_json.as_bytes()).map_err(|e| format!("JSON write error: {}", e))?;

        let status_json_val_str = match &status.action_status {
            Ok(code) => code.to_string(),
            Err(e_str) => format!("\"{}\"", escape_json_string(e_str)),
        };
        let status_json = format!("    \"status\": {},\n", status_json_val_str);
        writer.write_all(status_json.as_bytes()).map_err(|e| format!("JSON write error: {}", e))?;

        let response_time_json = format!(
            "    \"responseTimeMs\": {},\n",
            status.response_time.as_millis()
        );
        writer.write_all(response_time_json.as_bytes()).map_err(|e| format!("JSON write error: {}", e))?;

        let timestamp_epoch_s = status.timestamp.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let timestamp_json = format!("    \"timestampEpochS\": {}\n", timestamp_epoch_s);
        writer.write_all(timestamp_json.as_bytes()).map_err(|e| format!("JSON write error: {}", e))?;

        writer.write_all(b"  }").map_err(|e| format!("JSON write error: {}", e))?;
        if i < statuses.len() - 1 {
            writer.write_all(b",\n").map_err(|e| format!("JSON write error: {}", e))?;
        } else {
            writer.write_all(b"\n").map_err(|e| format!("JSON write error: {}", e))?;
        }
    }

    writer.write_all(b"]\n").map_err(|e| format!("JSON write error: {}", e))?;
    writer.flush().map_err(|e| format!("JSON flush error: {}", e))?;
    Ok(())
}