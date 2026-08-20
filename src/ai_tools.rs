use crate::config::Config;
use crate::util::ask;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{json, Value};
use tr::tr;

/// Set once the first mode prompt has been shown, so later tool sessions in the
/// same process do not nag again.
static MODE_PROMPT_SHOWN: AtomicBool = AtomicBool::new(false);

/// A live viewport the ai can draw into: the tool mode on the first line, the
/// `##` tool status lines in the middle, and the spinner with a phrase last.
/// Implemented by [`crate::ai::LiveView`]; when the terminal is not interactive
/// the tools fall back to plain stdout lines instead.
pub trait ToolView {
    /// Updates the mode shown on the first line.
    fn set_mode(&mut self, label: &str);
    /// Adds a fresh `##` tool line below the thinking.
    fn push_tool_line(&mut self, line: String);
    /// Replaces the wording of the last `##` tool line in place.
    fn rewrite_last_tool_line(&mut self, line: String);
    /// Reads a single y/n key (no Enter needed); true is yes.
    fn confirm(&mut self) -> Result<bool>;
    /// Asks a `::` question with the viewport parked, returning the answer.
    fn ask(&mut self, question: &str, default: bool) -> bool;
}

/// How strictly tool calls are confirmed before they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolMode {
    /// Run every tool without asking.
    Auto,
    /// Ask before `web_fetch` only; search tools run freely.
    #[default]
    FetchConfirm,
    /// Ask before every tool call.
    AllConfirm,
}

/// The present and past tense label of a tool call, plus its keyword, so the
/// `##` line can show what the ai is doing and what it finished doing.
struct ToolWords {
    present: String,
    past: String,
    keyword: String,
}

fn tool_words(name: &str, args: &Value) -> ToolWords {
    let keyword = args
        .get("query")
        .or_else(|| args.get("url"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let (present, past) = match name {
        "search_packages" => (tr!("Searching for packages"), tr!("Searched for packages")),
        "web_search" => (tr!("Searching the web"), tr!("Web Searched")),
        "web_fetch" => (tr!("Fetching"), tr!("Fetched")),
        _ => (name.to_string(), name.to_string()),
    };
    ToolWords {
        present,
        past,
        keyword,
    }
}

/// A colored `##` tool status line, optionally with the `? [Y/n]` suffix.
fn tool_line(config: &Config, verb: &str, keyword: &str, confirm: bool) -> String {
    let mut line = format!(
        "{} {}",
        config.color.action.paint("##"),
        config.color.bold.paint(verb),
    );
    if !keyword.is_empty() {
        line.push_str(&format!(
            " {}",
            config.color.code.paint(format!("\"{}\"", keyword))
        ));
    }
    if confirm {
        line.push_str(&format!(" {}", config.color.bold.paint(tr!("? [Y/n]"))));
    }
    line
}

impl ToolMode {
    pub fn label(self) -> &'static str {
        match self {
            ToolMode::Auto => "auto",
            ToolMode::FetchConfirm => "fetch-confirm",
            ToolMode::AllConfirm => "all-confirm",
        }
    }

    pub fn cycle(&mut self) {
        *self = match self {
            ToolMode::Auto => ToolMode::FetchConfirm,
            ToolMode::FetchConfirm => ToolMode::AllConfirm,
            ToolMode::AllConfirm => ToolMode::Auto,
        };
    }
}

/// The maximum number of characters a fetched page contributes.
const MAX_FETCH: usize = 12_000;

/// Executes the tools the ai may call, prompting the user when the mode asks.
pub struct ToolExecutor<'a> {
    config: &'a Config,
    mode: ToolMode,
}

impl<'a> ToolExecutor<'a> {
    pub fn new(config: &'a Config) -> Self {
        let mut mode = ToolMode::default();
        if config.no_confirm {
            // Non interactive runs have nobody to answer the prompt.
            mode = ToolMode::Auto;
        }
        Self { config, mode }
    }

    fn needs_confirm(&self, name: &str) -> bool {
        if self.config.no_confirm {
            return false;
        }
        match self.mode {
            ToolMode::Auto => false,
            ToolMode::FetchConfirm => name == "web_fetch",
            ToolMode::AllConfirm => true,
        }
    }

    /// The OpenAI tools declaration, describing every tool to the model.
    pub fn schema() -> Value {
        json!([
            {
                "type": "function",
                "function": {
                    "name": "search_packages",
                    "description": tr!("Search the Arch Linux pacman repositories and the AUR for packages matching a query. Returns a numbered list of name, repo, version and description.").to_string(),
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": tr!("the search term").to_string()
                            }
                        },
                        "required": ["query"]
                    }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "web_search",
                    "description": tr!("Search the web for recent information. Uses the configured Tavily API when available, otherwise falls back to scraping public search pages.").to_string(),
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": tr!("the search query").to_string()
                            }
                        },
                        "required": ["query"]
                    }
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "web_fetch",
                    "description": tr!("Fetch a web page and return its readable text. Use to read a specific URL returned by web_search.").to_string(),
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "url": {
                                "type": "string",
                                "description": tr!("the http(s) url to fetch").to_string()
                            }
                        },
                        "required": ["url"]
                    }
                }
            }
        ])
    }

    /// Runs one tool call, returning the text handed back to the model. When a
    /// live viewport is present the `##` status lines are drawn into it,
    /// otherwise they go to plain stdout.
    pub async fn run(
        &mut self,
        name: &str,
        args: &Value,
        view: Option<&mut dyn ToolView>,
    ) -> Result<String> {
        let words = tool_words(name, args);
        let confirm = self.needs_confirm(name);

        if let Some(view) = view {
            if confirm {
                view.push_tool_line(tool_line(self.config, &words.present, &words.keyword, true));
                if !view.confirm()? {
                    if self.mode == ToolMode::AllConfirm
                        && view.ask(&tr!("Switch to automatic tool execution? [Y/n]",), true)
                    {
                        self.mode = ToolMode::Auto;
                        view.rewrite_last_tool_line(tool_line(
                            self.config,
                            &words.present,
                            &words.keyword,
                            false,
                        ));
                    } else {
                        view.rewrite_last_tool_line(tool_line(
                            self.config,
                            &words.past,
                            &words.keyword,
                            false,
                        ));
                        return Ok(json!({
                            "status": "declined",
                            "message": tr!("the user declined to run this tool").to_string(),
                        })
                        .to_string());
                    }
                } else {
                    view.rewrite_last_tool_line(tool_line(
                        self.config,
                        &words.present,
                        &words.keyword,
                        false,
                    ));
                }
            } else {
                view.push_tool_line(tool_line(
                    self.config,
                    &words.present,
                    &words.keyword,
                    false,
                ));
            }

            let result = self.run_tool(name, args).await;
            view.rewrite_last_tool_line(tool_line(self.config, &words.past, &words.keyword, false));
            return result;
        }

        if confirm {
            if !self.confirm_tool(&words).await? {
                self.rewrite_tool_line(&words.past, &words.keyword, true);
                return Ok(json!({
                    "status": "declined",
                    "message": tr!("the user declined to run this tool").to_string(),
                })
                .to_string());
            }
            // The confirmation rewrote the line to the present tense; nothing
            // more to do before running.
        } else {
            self.print_tool_line(&words.present, &words.keyword, false);
        }

        let result = self.run_tool(name, args).await;
        self.rewrite_tool_line(&words.past, &words.keyword, true);
        result
    }

    async fn run_tool(&self, name: &str, args: &Value) -> Result<String> {
        match name {
            "search_packages" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.search_packages(query).await
            }
            "web_search" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.web_search(query).await
            }
            "web_fetch" => {
                let url = args.get("url").and_then(Value::as_str).unwrap_or_default();
                self.web_fetch(url).await
            }
            _ => bail!(tr!("ai asked for an unavailable tool: {}", name)),
        }
    }

    /// Prints a fresh `##` tool line. When `confirm` is set the `? [Y/n]`
    /// suffix is appended and the line is not ended, so the confirmation can
    /// rewrite it in place.
    fn print_tool_line(&self, verb: &str, keyword: &str, confirm: bool) {
        print!("{}\r", tool_line(self.config, verb, keyword, confirm));
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    /// Overwrites the last `##` tool line with the final wording. A trailing
    /// newline is written so the next line starts clean.
    fn rewrite_tool_line(&self, verb: &str, keyword: &str, newline: bool) {
        print!(
            "\x1b[2K{}\r{}",
            tool_line(self.config, verb, keyword, false),
            if newline { "\n" } else { "" }
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    /// Asks on the `##` line itself whether to run a tool: the `? [Y/n]`
    /// suffix is appended, the answer is read, then the suffix disappears and
    /// the line stays in the present tense until the tool has finished.
    /// Returns false to decline.
    async fn confirm_tool(&mut self, words: &ToolWords) -> Result<bool> {
        use std::io::{BufRead, Write};

        let mut line = format!(
            "{} {}",
            self.config.color.action.paint("##"),
            self.config.color.bold.paint(words.present.clone()),
        );
        if !words.keyword.is_empty() {
            line.push_str(&format!(
                " {}",
                self.config
                    .color
                    .code
                    .paint(format!("\"{}\"", words.keyword))
            ));
        }
        print!("{} {} ", line, self.config.color.bold.paint(tr!("? [Y/n]")));
        let _ = std::io::stdout().lock().flush();

        let stdin = std::io::stdin();
        let mut input = String::new();
        let _ = stdin.lock().read_line(&mut input);
        let input = input.trim().to_lowercase();

        let yes = input == tr!("y") || input == tr!("yes") || input.is_empty();
        if yes {
            // The [Y/n] suffix disappears once the choice is made.
            self.rewrite_tool_line(&words.present, &words.keyword, false);
            return Ok(true);
        }

        if self.mode == ToolMode::AllConfirm {
            // Offer switching to auto so the user is not stuck answering
            // every call.
            if ask(
                self.config,
                &tr!("Switch to automatic tool execution? [Y/n]",),
                true,
            ) {
                self.mode = ToolMode::Auto;
                self.rewrite_tool_line(&words.present, &words.keyword, false);
                return Ok(true);
            }
        }

        self.rewrite_tool_line(&words.present, &words.keyword, false);
        Ok(false)
    }

    /// Prompts the user to switch tool modes with Tab, returning the mode.
    /// The live viewport's first line shows the mode and updates in place as it
    /// cycles, so nothing extra is printed. Returns `None` when the user
    /// pressed Esc to interrupt.
    ///
    /// Only the very first tool session of the process shows the prompt; later
    /// sessions reuse the mode silently. Returns immediately without prompting
    /// when confirmation is off.
    pub fn choose(&mut self, mut view: Option<&mut dyn ToolView>) -> Option<ToolMode> {
        if self.config.no_confirm {
            return Some(self.mode);
        }

        if MODE_PROMPT_SHOWN.swap(true, Ordering::SeqCst) {
            return Some(self.mode);
        }

        // Raw mode so Tab and Esc arrive without Enter; the live view keeps
        // drawing over it.
        let _ = terminal::enable_raw_mode();
        let result = loop {
            if let Some(view) = view.as_mut() {
                view.set_mode(self.mode.label());
            }
            match key_pressed(Duration::from_millis(600)) {
                Some(KeyCode::Tab) => self.mode.cycle(),
                Some(KeyCode::Esc) => break None,
                _ => break Some(self.mode),
            }
        };
        let _ = terminal::disable_raw_mode();
        result
    }
}

/// A readable page: title plus the text of the main paragraphs.
fn text_from_html(html: &str, max: usize) -> String {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);
    let mut out = String::new();

    let title = Selector::parse("title").ok();
    if let Some(sel) = title {
        if let Some(t) = doc.select(&sel).next() {
            let t = t.text().collect::<String>().trim().to_string();
            if !t.is_empty() {
                out.push_str(&format!("{}{}\n", "title: ", t));
            }
        }
    }

    for sel in [
        Selector::parse("main"),
        Selector::parse("article"),
        Selector::parse("body"),
    ]
    .into_iter()
    .flatten()
    {
        let mut count = 0;
        for node in doc.select(&sel) {
            for text in node.text() {
                let text = text.trim();
                if !text.is_empty() {
                    out.push_str(text);
                    out.push('\n');
                }
                count += 1;
                if out.len() >= max {
                    break;
                }
            }
            if out.len() >= max {
                break;
            }
            let _ = count;
        }
        if !out.is_empty() {
            break;
        }
    }

    if out.len() > max {
        out.truncate(max);
    }
    out.trim().to_string()
}

/// Parses results out of DuckDuckGo's html endpoint.
fn parse_ddg(html: &str, max: usize) -> Vec<(String, String)> {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);
    let result_sel = Selector::parse("div.result").unwrap();
    let a_sel = Selector::parse("a.result__a").unwrap();
    let snip_sel = Selector::parse("a.result__snippet").unwrap();

    let mut results = Vec::new();
    for node in doc.select(&result_sel) {
        let mut title = String::new();
        let mut url = String::new();
        if let Some(a) = node.select(&a_sel).next() {
            title = a.text().collect::<String>().trim().to_string();
            url = a.value().attr("href").unwrap_or_default().to_string();
        }
        let snippet = node
            .select(&snip_sel)
            .next()
            .map(|s| s.text().collect::<String>().trim().to_string())
            .unwrap_or_default();
        if !title.is_empty() && !url.is_empty() {
            results.push((title, format!("{}\n{}", url, snippet).trim().to_string()));
        }
        if results.len() >= max {
            break;
        }
    }
    results
}

/// Parses results out of cn.bing's html endpoint.
fn parse_bing(html: &str, max: usize) -> Vec<(String, String)> {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);
    let item_sel = Selector::parse("li.b_algo").unwrap();
    let a_sel = Selector::parse("h2 a").unwrap();
    let snip_sel = Selector::parse("p").unwrap();

    let mut results = Vec::new();
    for node in doc.select(&item_sel) {
        let mut title = String::new();
        let mut url = String::new();
        if let Some(a) = node.select(&a_sel).next() {
            title = a.text().collect::<String>().trim().to_string();
            url = a.value().attr("href").unwrap_or_default().to_string();
        }
        let snippet = node
            .select(&snip_sel)
            .next()
            .map(|s| s.text().collect::<String>().trim().to_string())
            .unwrap_or_default();
        if !title.is_empty() && !url.is_empty() {
            results.push((title, format!("{}\n{}", url, snippet).trim().to_string()));
        }
        if results.len() >= max {
            break;
        }
    }
    results
}

impl ToolExecutor<'_> {
    /// Searches pacman repositories and the AUR for a query.
    async fn search_packages(&self, query: &str) -> Result<String> {
        let query = query.trim();
        ensure!(!query.is_empty(), tr!("search query is empty"));

        let mut out = Vec::new();
        let mut n = 0;

        if self.config.mode.repo() {
            let targets = vec![query.to_string()];
            if let Ok(pkgs) = crate::search::search_repos(self.config, &targets) {
                for pkg in pkgs {
                    n += 1;
                    let db = pkg.db().map(|d| d.name()).unwrap_or("repo");
                    let desc = pkg.desc().unwrap_or_default();
                    out.push(format!(
                        "{}. {}/{} {}  {}",
                        n,
                        db,
                        pkg.name(),
                        pkg.version(),
                        desc
                    ));
                }
            }
        }

        if self.config.mode.aur() {
            let targets = vec![query.to_string()];
            if let Ok(pkgs) = crate::search::search_aur(self.config, &targets).await {
                for pkg in pkgs {
                    n += 1;
                    let desc = pkg.description.clone().unwrap_or_default();
                    let extra =
                        format!(" votes={} popularity={:.2}", pkg.num_votes, pkg.popularity);
                    out.push(format!(
                        "{}. aur/{} {}  {}{}",
                        n, pkg.name, pkg.version, desc, extra
                    ));
                }
            }
        }

        if out.is_empty() {
            return Ok(json!({
                "status": "no results",
                "message": tr!("no packages matched the query in the repositories or the AUR").to_string(),
            })
            .to_string());
        }

        Ok(out.join("\n"))
    }

    /// Web search: Tavily when configured, otherwise scrape public engines.
    async fn web_search(&self, query: &str) -> Result<String> {
        let query = query.trim();
        ensure!(!query.is_empty(), tr!("search query is empty"));

        if !self.config.ai_tavily_key.is_empty() {
            return self.tavily_search(query).await;
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.config.ai_timeout))
            .build()?;

        let mut results = Vec::new();

        // DuckDuckGo html endpoint.
        let ddg_url = format!("https://html.duckduckgo.com/html/?q={}", urlencode(query));
        if let Ok(resp) = client.get(&ddg_url).send().await {
            if resp.status().is_success() {
                if let Ok(html) = resp.text().await {
                    results.extend(parse_ddg(&html, 5));
                }
            }
        }

        // cn.bing.
        let bing_url = format!("https://cn.bing.com/search?q={}", urlencode(query));
        if let Ok(resp) = client.get(&bing_url).send().await {
            if resp.status().is_success() {
                if let Ok(html) = resp.text().await {
                    results.extend(parse_bing(&html, 5));
                }
            }
        }

        // De-duplicate by title.
        results.sort_by_key(|(t, _)| t.clone());
        results.dedup_by(|a, b| a.0 == b.0);

        if results.is_empty() {
            return Ok(json!({
                "status": "no results",
                "message": tr!("web search returned no results").to_string(),
            })
            .to_string());
        }

        let mut out = String::new();
        for (title, rest) in results.into_iter().take(10) {
            out.push_str(&format!("- {}\n  {}\n", title, rest));
        }
        Ok(out.trim().to_string())
    }

    async fn tavily_search(&self, query: &str) -> Result<String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.config.ai_timeout))
            .build()?;

        let body = json!({
            "api_key": self.config.ai_tavily_key.expose(),
            "query": query,
            "search_depth": "basic",
            "max_results": 6,
            "include_answer": true,
        });

        let resp = client
            .post("https://api.tavily.com/search")
            .json(&body)
            .send()
            .await
            .with_context(|| tr!("failed to query tavily"))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("{}: {}", tr!("tavily request failed"), body);
        }

        let value: Value = resp.json().await?;

        let mut out = String::new();
        if let Some(answer) = value.get("answer").and_then(Value::as_str) {
            if !answer.is_empty() {
                out.push_str(&format!("Answer: {}\n\n", answer));
            }
        }

        let results = value.get("results").and_then(Value::as_array);
        if let Some(results) = results {
            if results.is_empty() {
                return Ok(json!({
                    "status": "no results",
                    "message": tr!("web search returned no results").to_string(),
                })
                .to_string());
            }
            for r in results.iter().take(6) {
                let title = r.get("title").and_then(Value::as_str).unwrap_or_default();
                let url = r.get("url").and_then(Value::as_str).unwrap_or_default();
                let content = r.get("content").and_then(Value::as_str).unwrap_or_default();
                out.push_str(&format!("- {}\n  {}\n  {}\n", title, url, content));
            }
        }

        Ok(out.trim().to_string())
    }

    /// Fetches a page and returns its readable text.
    async fn web_fetch(&self, url: &str) -> Result<String> {
        let url = url.trim();
        ensure!(
            url.starts_with("https://") || url.starts_with("http://"),
            tr!("only http(s) urls can be fetched")
        );

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("pwa (paru with ai)"));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.config.ai_timeout))
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;

        let resp = client
            .get(url)
            .send()
            .await
            .with_context(|| tr!("failed to fetch: {}", url))?;
        if !resp.status().is_success() {
            bail!("{}: {}", url, resp.status());
        }

        let html = resp
            .text()
            .await
            .with_context(|| tr!("failed to read: {}", url))?;

        let text = text_from_html(&html, MAX_FETCH);
        if text.is_empty() {
            return Ok(json!({
                "status": "empty",
                "message": tr!("the page did not contain readable text").to_string(),
            })
            .to_string());
        }

        Ok(json!({
            "url": url,
            "content": text,
        })
        .to_string())
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Polls for a Tab, Esc or Enter keypress; returns what was pressed.
fn key_pressed(timeout: Duration) -> Option<KeyCode> {
    match event::poll(timeout) {
        Ok(true) => match event::read() {
            Ok(Event::Key(key)) => {
                if key.code == KeyCode::Tab
                    || (key.code == KeyCode::Char('i')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    Some(KeyCode::Tab)
                } else {
                    Some(key.code)
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// Non blocking check for an Esc keypress, used to interrupt a streaming reply.
pub fn esc_pressed() -> bool {
    matches!(key_pressed(Duration::from_millis(0)), Some(KeyCode::Esc))
}

/// Reads a single y/n answer from the terminal, no Enter needed. `true` is
/// yes, `false` no.
pub(crate) fn read_yn_key() -> Result<bool> {
    terminal::enable_raw_mode()?;
    let result = loop {
        match event::read() {
            Ok(Event::Key(key)) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => break true,
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => break false,
                _ => {}
            },
            Err(_) => break false,
            _ => {}
        }
    };
    let _ = terminal::disable_raw_mode();
    Ok(result)
}
