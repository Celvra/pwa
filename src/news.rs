use crate::config::Config;
use crate::fmt::print_indent;

use std::str::Chars;

use ansiterm::Style;
use anyhow::{bail, Result};
use htmlescape::decode_html;
use rss::Channel;
use tr::tr;

enum Tag {
    CodeOpen,
    CodeClose,
    PClose,
    Other,
}

pub fn newest_pkg(config: &Config) -> i64 {
    let max = config
        .alpm
        .localdb()
        .pkgs()
        .iter()
        .map(|p| p.build_date())
        .max()
        .unwrap_or_default();

    max
}

pub async fn news(config: &Config) -> Result<i32> {
    let url = config.arch_url.join("feeds/news")?;
    let client = config.raur.client();

    let resp = client.get(url.clone()).send().await?;
    if !resp.status().is_success() {
        bail!("{}: {}", url, resp.status());
    }
    let bytes = resp.bytes().await?;
    let channel = Channel::read_from(bytes.as_ref())?;
    let c = config.color;

    let mut printed = false;

    for item in channel.into_items().into_iter().rev() {
        let date = item.pub_date().unwrap_or_default();

        match chrono::DateTime::parse_from_rfc2822(date) {
            Ok(date) => {
                if config.news < 2 && date.timestamp() < newest_pkg(config) {
                    continue;
                }

                print!("{} ", c.news_date.paint(date.format("%F").to_string()));
            }
            Err(_) => print!("{}", tr!("No Date ")),
        }

        let no_title = tr!("No Title");
        let title = item.title().unwrap_or(no_title.as_str());
        println!("{}", c.bold.paint(title));

        printed = true;
        parse_html(config, item.description().unwrap_or_default());
    }

    if !printed {
        eprintln!("{}", tr!("no new news"));
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Recent news as plain text, for feeding the ai a grounding corpus.
pub async fn news_text(config: &Config) -> Result<String> {
    let url = config.arch_url.join("feeds/news")?;
    let client = config.raur.client();

    let resp = client.get(url.clone()).send().await?;
    if !resp.status().is_success() {
        bail!("{}: {}", url, resp.status());
    }
    let bytes = resp.bytes().await?;
    let channel = Channel::read_from(bytes.as_ref())?;

    let mut out = String::new();

    for item in channel.into_items().into_iter().rev() {
        let date = item.pub_date().unwrap_or_default();

        match chrono::DateTime::parse_from_rfc2822(date) {
            Ok(date) => {
                if config.news < 2 && date.timestamp() < newest_pkg(config) {
                    continue;
                }
                let _ = std::fmt::Write::write_fmt(
                    &mut out,
                    format_args!("{} ", date.format("%F")),
                );
            }
            Err(_) => out.push_str("(no date) "),
        }

        let no_title = tr!("No Title");
        let title = item.title().unwrap_or(no_title.as_str());
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{}\n", title));

        let desc = item.description().unwrap_or_default();
        let text = html_to_text(desc);
        out.push_str(&text);
        out.push('\n');
    }

    Ok(out.trim().to_string())
}

fn html_to_text(html: &str) -> String {
    let mut words = String::with_capacity(html.len());
    let mut chars = html.chars();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            for c in chars.by_ref() {
                if c == '>' {
                    break;
                }
                tag.push(c);
            }
            if tag == "/p" {
                words.push('\n');
            } else if tag == "/code" {
                words.push(' ');
            }
        } else {
            words.push(c);
        }
    }
    let mut words = decode_html(&words).unwrap_or(words);
    // Collapse runs of blank lines for a compact corpus.
    while words.contains("\n\n\n") {
        words = words.replace("\n\n\n", "\n\n");
    }
    words.trim().to_string()
}

fn parse_html(config: &Config, html: &str) {
    let code = config.color.code;
    let mut words = String::with_capacity(html.len());

    let mut chars = html.chars();
    while let Some(c) = chars.next() {
        if c == '<' {
            let tag = parse_tag(&mut chars);

            match tag {
                Tag::CodeOpen => {
                    words.push(' ');
                    words.push_str(&code.prefix().to_string());
                }
                Tag::CodeClose => words.push_str(&code.suffix().to_string()),
                Tag::PClose => words.push('\n'),
                Tag::Other => (),
            }
        } else {
            words.push(c);
        }
    }

    words.push_str(&code.suffix().to_string());
    let words = words;
    let words = decode_html(&words).unwrap_or(words);

    for line in words.lines() {
        print!("    ");
        let line = line.split_whitespace();
        print_indent(Style::new(), 4, 4, config.cols, " ", line);
    }
}

fn parse_tag(iter: &mut Chars) -> Tag {
    if iter.as_str().starts_with("code>") {
        iter.by_ref().take(5).count();
        Tag::CodeOpen
    } else if iter.as_str().starts_with("/code>") {
        iter.by_ref().take(6).count();
        Tag::CodeClose
    } else if iter.as_str().starts_with("/p>") {
        iter.by_ref().take(3).count();
        Tag::PClose
    } else {
        iter.by_ref().any(|c| c == '>');
        Tag::Other
    }
}
