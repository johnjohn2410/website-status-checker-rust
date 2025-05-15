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
    action_status: Result<u16, String>,
    response_time: Duration,
    timestamp: SystemTime,
}

// Struct to hold configuration (updated)
#[derive(Debug, Clone)]
struct Config {
    timeout: Duration,
    retries: u32,
    header_assertion: Option<(String, String)>, // For --assert-header "Name:Value"
}

// Struct for round statistics (Bonus Feature)
#[derive(Debug, Default)]
struct RoundStats {
    min_time: Option<Duration>,
    max_time: Option<Duration>,
    total_time: Duration,
    successful_checks: u64,
    failed_checks: u64,
}

impl RoundStats {
    fn new() -> Self {
        Default::default()
    }

    fn update(&mut self, status: &WebsiteStatus) {
        match status.action_status {
            Ok(_) => {
                self.successful_checks += 1;
                if self.successful_checks == 1 {
                    self.min_time = Some(status.response_time);
                    self.max_time = Some(status.response_time);
                } else {
                    if self.min_time.map_or(true, |min| status.response_time < min) {
                        self.min_time = Some(status.response_time);
                    }
                    if self.max_time.map_or(true, |max| status.response_time > max) {
                        self.max_time = Some(status.response_time);
                    }
                }
                self.total_time += status.response_time;
            }
            Err(_) => {
                self.failed_checks += 1;
            }
        }
    }

    fn print_summary(&self) {
        println!("\n--- Round Summary ---");
        let total_attempted = self.successful_checks + self.failed_checks;
        println!("Total URLs Attempted: {}", total_attempted);
        println!("Successful Checks: {}", self.successful_checks);
        println!("Failed Checks: {}", self.failed_checks);

        if self.successful_checks > 0 {
            if let Some(min) = self.min_time {
                println!("Min Response Time (successful): {} ms", min.as_millis());
            }
            if let Some(max) = self.max_time {
                println!("Max Response Time (successful): {} ms", max.as_millis());
            }
            if self.total_time > Duration::ZERO {
                let avg_time_ms = self.total_time.as_millis() as f64 / self.successful_checks as f64;
                println!("Average Response Time (successful): {:.2} ms", avg_time_ms);
            }
        } else if total_attempted > 0 {
            println!("No successful checks to calculate response time statistics.");
        }
        println!("---------------------\n");
    }
}


fn main() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();

    let mut initial_urls_to_check: Vec<String> = Vec::new();
    let mut file_path: Option<String> = None;
    let mut num_workers: usize = std::thread::available_parallelism().map_or(2, |nz| nz.get());
    let mut timeout_seconds: u64 = 5;
    let mut retries_count: u32 = 0;
    let mut period_seconds: Option<u64> = None;
    let mut header_assertion_str: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
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
                    num_workers = args[i].parse().map_err(|_| format!("Invalid number for --workers: {}", args[i]))?;
                    if num_workers == 0 { return Err("--workers must be at least 1".to_string()); }
                } else {
                    return Err("--workers requires an argument".to_string());
                }
            }
            "--timeout" => {
                i += 1;
                if i < args.len() {
                    timeout_seconds = args[i].parse().map_err(|_| format!("Invalid number for --timeout: {}", args[i]))?;
                    if timeout_seconds == 0 { return Err("--timeout must be at least 1 second".to_string()); }
                } else {
                    return Err("--timeout requires an argument".to_string());
                }
            }
            "--retries" => {
                i += 1;
                if i < args.len() {
                    retries_count = args[i].parse().map_err(|_| format!("Invalid number for --retries: {}", args[i]))?;
                } else {
                    return Err("--retries requires an argument".to_string());
                }
            }
            "--period" => {
                i += 1;
                if i < args.len() {
                    let p_val = args[i].parse().map_err(|_| format!("Invalid number for --period: {}", args[i]))?;
                    if p_val == 0 { return Err("--period must be at least 1 second".to_string()); }
                    period_seconds = Some(p_val);
                } else {
                    return Err("--period requires an argument".to_string());
                }
            }
            "--assert-header" => {
                i += 1;
                if i < args.len() {
                    header_assertion_str = Some(args[i].clone());
                } else {
                    return Err("--assert-header requires an argument in 'Name: Value' format".to_string());
                }
            }
            "-h" | "--help" => {
                print_usage(&args[0]);
                return Ok(());
            }
            s if s.starts_with("--") => {
                return Err(format!("Unknown option: {}", s));
            }
            s => {
                initial_urls_to_check.push(s.to_string());
            }
        }
        i += 1;
    }

    let parsed_header_assertion: Option<(String, String)> = match header_assertion_str {
        Some(s) => {
            let parts: Vec<&str> = s.splitn(2, ':').collect();
            if parts.len() == 2 {
                let name = parts[0].trim().to_string();
                let value = parts[1].trim().to_string();
                if name.is_empty() || value.is_empty() {
                    return Err("Invalid format for --assert-header: Name and Value cannot be empty. Use 'Header-Name: Expected Value'".to_string());
                }
                Some((name.to_lowercase(), value))
            } else {
                return Err("Invalid format for --assert-header. Use 'Header-Name: Expected Value'".to_string());
            }
        }
        None => None,
    };

    if let Some(path_str) = &file_path {
        let path = Path::new(path_str.as_str());
        let file = File::open(path).map_err(|e| format!("Failed to open file {}: {}", path_str, e))?;
        let reader = io::BufReader::new(file);
        for line_result in reader.lines() {
            let line = line_result.map_err(|e| format!("Failed to read line from file: {}", e))?;
            let line_without_comment = if let Some(comment_start) = line.find('#') {
                if comment_start == 0 { "" } else { &line[..comment_start] }
            } else { &line };
            let trimmed_url_part = line_without_comment.trim();
            if !trimmed_url_part.is_empty() {
                initial_urls_to_check.push(trimmed_url_part.to_string());
            }
        }
    }

    if initial_urls_to_check.is_empty() {
        print_usage(&args[0]);
        eprintln!("\nError: No URLs provided via --file or positional arguments.");
        std::process::exit(2);
    }

    let mut seen_urls_master = std::collections::HashSet::new();
    initial_urls_to_check.retain(|url| seen_urls_master.insert(url.clone()));

    let base_config = Config {
        timeout: Duration::from_secs(timeout_seconds),
        retries: retries_count,
        header_assertion: parsed_header_assertion,
    };

    let client = Arc::new(
        reqwest::blocking::Client::builder()
            .timeout(base_config.timeout)
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?,
    );

    let mut round_counter = 0;
    loop {
        round_counter += 1;
        if period_seconds.is_some() {
            println!("--- Starting Round {} ---", round_counter);
        }

        let current_round_urls = initial_urls_to_check.clone();
        if current_round_urls.is_empty() {
            if period_seconds.is_none() { break; }
            println!("No URLs to check in this round. Waiting for next period if applicable.");
            if let Some(seconds) = period_seconds {
                thread::sleep(Duration::from_secs(seconds));
                continue;
            } else { break; }
        }

        let jobs_queue = Arc::new(Mutex::new(VecDeque::from(current_round_urls.clone())));
        let num_total_jobs_this_round = current_round_urls.len();

        let (result_tx, result_rx): (Sender<WebsiteStatus>, Receiver<WebsiteStatus>) = channel();
        let config_for_round = Arc::new(base_config.clone());

        let mut worker_handles = Vec::new();
        for worker_id in 0..num_workers {
            let jobs_queue_clone = Arc::clone(&jobs_queue);
            let result_tx_clone = result_tx.clone();
            let client_clone = Arc::clone(&client);
            let config_clone = Arc::clone(&config_for_round);

            let handle = thread::spawn(move || {
                loop {
                    let url_to_check: String = match jobs_queue_clone.lock() {
                        Ok(mut queue_guard) => {
                            if let Some(url) = queue_guard.pop_front() { url } else { break; }
                        }
                        Err(p) => { eprintln!("Worker {}: job queue mutex poisoned: {}", worker_id, p); break; }
                    };

                    let mut final_status_result_action: Result<u16, String> = Err("Worker failed to determine status".to_string());
                    let mut final_response_time = Duration::from_secs(0);
                    let mut final_timestamp = SystemTime::now();

                    for attempt in 0..=(config_clone.retries) {
                        let start_time = Instant::now();
                        let request_result = client_clone.get(&url_to_check).send();

                        final_response_time = start_time.elapsed();
                        final_timestamp = SystemTime::now();

                        match request_result {
                            Ok(response) => {
                                let status_code = response.status().as_u16();
                                if let Some((assert_name, assert_value)) = &config_clone.header_assertion {
                                    let found_header = response.headers().iter()
                                        .find(|(name, _)| name.as_str().to_lowercase() == *assert_name);

                                    match found_header {
                                        Some((_, actual_value_header)) => {
                                            match actual_value_header.to_str() {
                                                Ok(actual_value_str) if actual_value_str == assert_value => {
                                                    final_status_result_action = Ok(status_code);
                                                }
                                                Ok(actual_value_str) => {
                                                    final_status_result_action = Err(format!(
                                                        "Header '{}' assertion failed: expected '{}', got '{}'",
                                                        assert_name, assert_value, actual_value_str
                                                    ));
                                                }
                                                Err(_) => {
                                                    final_status_result_action = Err(format!(
                                                        "Header '{}' assertion failed: actual value not valid UTF-8: {:?}",
                                                        assert_name, actual_value_header
                                                    ));
                                                }
                                            }
                                        }
                                        None => {
                                            final_status_result_action = Err(format!(
                                                "Header '{}' assertion failed: header not found",
                                                assert_name
                                            ));
                                        }
                                    }
                                } else {
                                    final_status_result_action = Ok(status_code);
                                }
                                break;
                            }
                            Err(e) => {
                                final_status_result_action = Err(e.to_string());
                                if attempt >= config_clone.retries { break; }
                                if attempt < config_clone.retries { thread::sleep(Duration::from_millis(100));}
                            }
                        }
                    }

                    let status_to_send = WebsiteStatus {
                        url: url_to_check.clone(),
                        action_status: final_status_result_action,
                        response_time: final_response_time,
                        timestamp: final_timestamp,
                    };

                    if result_tx_clone.send(status_to_send).is_err() { break; }
                }
            });
            worker_handles.push(handle);
        }

        drop(result_tx);

        let mut all_statuses_this_round: Vec<WebsiteStatus> = Vec::with_capacity(num_total_jobs_this_round);
        let mut round_stats = RoundStats::new();

        if round_counter == 1 || period_seconds.is_some() {
            println!(
                "{:<30} | {:<8} | {:<12} | {}",
                "URL", "Status", "Time (ms)", "Timestamp (EpochS)"
            );
            println!("{}", "-".repeat(75));
        }

        for _ in 0..num_total_jobs_this_round {
            match result_rx.recv() {
                Ok(status) => {
                    round_stats.update(&status);
                    let status_str = match &status.action_status {
                        Ok(code) => code.to_string(),
                        Err(e_str) => {
                            if e_str.len() > 20 { format!("ERR: {}...", &e_str[..17]) } else { format!("ERR: {}", e_str) }
                        }
                    };
                    let time_ms = status.response_time.as_millis();
                    let timestamp_epoch_s = status.timestamp.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                    println!(
                        "{:<30} | {:<8} | {:<12} | {}",
                        truncate_url(&status.url, 28), status_str, time_ms, timestamp_epoch_s
                    );
                    all_statuses_this_round.push(status);
                }
                Err(_) => { break; }
            }
        }

        for (i,handle) in worker_handles.into_iter().enumerate() {
            if handle.join().is_err() { eprintln!("Error: Worker thread {} panicked.", i); }
        }

        if !all_statuses_this_round.is_empty() {
            let json_filename = if period_seconds.is_some() {
                format!("status_round_{}.json", round_counter)
            } else {
                "status.json".to_string()
            };
            write_json_output(&all_statuses_this_round, &json_filename)?;
            println!("\nResults for this round written to {}", json_filename);
        } else if num_total_jobs_this_round > 0 {
            println!("\nNo results were successfully processed in this round.");
        }

        round_stats.print_summary();

        if let Some(seconds) = period_seconds {
            if seconds > 0 {
                println!("Waiting for {} seconds before next round...\n", seconds);
                thread::sleep(Duration::from_secs(seconds));
            } else { break; }
        } else {
            break;
        }
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
    eprintln!("\nBonus Features:");
    eprintln!("  --period <seconds>   Loop forever, checking URLs every <seconds> interval (min 1).");
    eprintln!("                       JSON output will be named status_round_N.json for each round.");
    eprintln!("  --assert-header \"Name: Value\" Check for a specific HTTP header and its exact value.");
    eprintln!("                       (Header name matching is case-insensitive; value matching is case-sensitive).");
    eprintln!("                       If assertion fails, the URL status will be an error.");
    eprintln!("\nIf neither --file nor positional URLs are supplied, this message is shown and the program exits with code 2.");
    eprintln!("\nJSON Output Fields (in status.json or status_round_N.json):");
    eprintln!("  url (String):             The original URL checked.");
    eprintln!("  status (Number or String): HTTP status code (e.g., 200) if successful, or an error message string if failed (including header assertion failures).");
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