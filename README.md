# urlcheck

A small command-line tool that checks a list of URLs concurrently and reports
the HTTP status of each.

## Usage

```sh
urlcheck [FILE]
```

- `FILE` defaults to `urls.txt` in the current directory.
- Pass `-` to read URLs from stdin, e.g. `cat urls.txt | urlcheck -`.
- `urlcheck --help` prints usage.

### Input format

One URL per line. Blank lines are ignored, and lines starting with `#` are
comments. Only `http` and `https` URLs are accepted; anything else is reported
as an invalid URL and counted as a failure.

```text
# Production sites
https://www.example.com
https://www.example.org/health
```

### Output

Each URL is printed with its result, in input order, followed by a summary:

```text
https://www.example.com         STATUS 200
http://example.org              STATUS 200 (redirected to https://example.org/)
https://down.example.net        HTTP Error: error sending request: ...
not-a-url                       Invalid URL: relative URL without a base
```

Redirects are followed (up to 10); when the final URL differs from the one
requested, it is shown. Only 2xx statuses count as successes.

### Behavior

- Requests use `HEAD`, falling back to `GET` if the server responds 405 or 501.
- At most 32 requests run at once.
- Each request times out after 15 seconds (10 seconds to connect).

### Exit codes

| Code | Meaning                                       |
| ---- | --------------------------------------------- |
| 0    | Every URL returned a 2xx status               |
| 1    | At least one URL failed, errored or was invalid |
| 2    | Usage error, or the input file could not be read |
