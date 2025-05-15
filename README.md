# Website Status Checker (Rust)

A command-line utility written in Rust to concurrently check the availability and status of multiple websites.

## Features

*   Checks multiple websites in parallel using a configurable worker thread pool.
*   Accepts URLs from a file or directly as command-line arguments.
*   Configurable per-request timeout.
*   Optional retries for failed requests with a fixed delay.
*   Collects HTTP status code (or error message), response time, and a timestamp for each URL.
*   Provides immediate live output to `stdout` for each URL.
*   Generates a `status.json` file with detailed results for all checked URLs.
*   Ignores blank lines and lines starting with `#` in input files.
*   Handles inline comments in input files (text after `#` on a URL line is ignored).

## Build Instructions

1.  Ensure you have Rust installed (version 1.78 or later is recommended). You can get it from [rustup.rs](https://rustup.rs/).
2.  Clone this repository:
    ```bash
    git clone https://github.com/johnjohn2410/website-status-checker-rust.git
    cd website-status-checker-rust
    ```
3.  Build the project for release (optimized executable):
    ```bash
    cargo build --release
    ```
4.  The executable will be located at `target/release/website-status-checker-rust`.

## Usage

./target/release/website-status-checker-rust [OPTIONS] [URL...]

**Options:**
*   `--file <path>`: Path to a text file containing URLs (one per line).
    *   Lines starting with `#` (as the first character) and blank lines are ignored.
    *   Inline comments (text after a `#` on a line containing a URL) are also ignored.
*   `--workers <N>`: Number of worker threads (default: number of logical CPU cores, minimum 1).
*   `--timeout <seconds>`: Per-request timeout in seconds (default: 5, minimum 1).
*   `--retries <N>`: Number of additional attempts after a failure (default: 0). A 100ms pause occurs between attempts.
*   `--period <seconds>`: Loop forever, checking URLs every `<seconds>` interval. JSON output per round.
*   `--assert-header "Name: Value"`: Check for a specific HTTP header and its exact value.
*   `-h, --help`: Show the help message and exit.

If neither `--file` nor positional URLs are supplied, a help message is shown, and the program exits with code 2.

**Examples:**

1.  Check a few URLs directly:
    ```bash
    ./target/release/website-status-checker-rust https://www.rust-lang.org https://www.google.com http://thissitedoesnotexist123.com
    ```

2.  Check URLs from a file named `sites.txt`:
    ```bash
    # Example content for sites.txt:
    # https://example.com
    # http://example.org # This is an inline comment, will be ignored
    # # This whole line is a comment
    #
    # http://another-example.net

    ./target/release/website-status-checker-rust --file sites.txt
    ```

3.  Use more options:
    ```bash
    ./target/release/website-status-checker-rust --file sites.txt --workers 8 --timeout 10 --retries 2 https://additionalurl.com
    ```

## Concurrency Model

The program utilizes a fixed pool of `N` worker threads, configurable via the `--workers N` option (defaulting to the number of logical CPU cores). These worker threads pull URLs from a shared job queue. Each worker makes a blocking HTTP request for its assigned URL. This model allows the program to efficiently process a large list of URLs by parallelizing the network-bound work across the available workers, improving overall throughput compared to sequential checking.

## Bonus Features Implemented

This project includes the following optional bonus features:

1.  **Periodic Monitoring (`--period <seconds>`)**
    *   The `--period <seconds>` flag enables continuous monitoring. The program will execute a full round of checks for all specified URLs, print the results and summary statistics, then wait for the given number of seconds before starting the next round.
    *   When using `--period`, the JSON output files will be named `status_round_N.json` for each round `N` (e.g., `status_round_1.json`, `status_round_2.json`).
    *   Use `Ctrl+C` to stop the periodic checks.
    *   **Example:** `./target/release/website-status-checker-rust --file sites.txt --period 60` (checks every minute).

2.  **Summary Statistics**
    *   At the end of each round of checks (or at the end of a single run if not using `--period`), summary statistics are printed to `stdout`.
    *   These statistics include:
        *   Total URLs Attempted
        *   Number of Successful Checks
        *   Number of Failed Checks
        *   Minimum Response Time (for successful checks)
        *   Maximum Response Time (for successful checks)
        *   Average Response Time (for successful checks)

3.  **HTTP Header Assertions (`--assert-header "Header-Name: Expected Value"`)**
    *   The `--assert-header` flag allows you to specify a single HTTP header name and an expected value.
    *   For each URL, after a successful HTTP response (e.g., 200 OK), the program will check if the specified header exists and if its value exactly matches the expected value.
    *   Header name matching is performed case-insensitively.
    *   Header value matching is case-sensitive.
    *   If the assertion fails (header missing, or value mismatch), the `action_status` for that URL will be an `Err` detailing the assertion failure, even if the HTTP status code was otherwise successful.
    *   **Example:** `./target/release/website-status-checker-rust https://example.com --assert-header "Content-Type: text/html; charset=UTF-8"`
    *   **Example (failed assertion):** `./target/release/website-status-checker-rust https://example.com --assert-header "X-Made-Up-Header: nope"`

## JSON Output (`status.json`)

After all URLs are processed, a `status.json` file is generated in the current working directory. It contains an array of objects, where each object represents the result for a single URL.

**JSON Object Fields:**

*   `url` (String): The original URL that was checked (after stripping any inline comments from the input file).
*   `status` (Number or String):
    *   If the request was successful and received an HTTP response, this field will be the numeric HTTP status code (e.g., `200`, `404`, `500`).
    *   If the request failed due to a network error, timeout, DNS issue, or other problem before an HTTP status could be determined, this field will be a String containing the error message from the HTTP client.
*   `responseTimeMs` (Number): The total time taken for the final attempt of the request, in milliseconds.
*   `timestampEpochS` (Number): A Unix timestamp (seconds since January 1, 1970, UTC) indicating when the final attempt for this URL completed.

**Example `status.json` entry (Success):**
```json
{
  "url": "https://www.rust-lang.org",
  "status": 200,
  "responseTimeMs": 450,
  "timestampEpochS": 1747273500
}

{
  "url": "http://nonexistentdomain123.org",
  "status": "error sending request for url (http://nonexistentdomain123.org/)",
  "responseTimeMs": 1502,
  "timestampEpochS": 1747273501
}

