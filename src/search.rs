use std::fmt::Write as _;
use std::path::Path;

use crate::config::SortBy;
use crate::config::{Config, SortMode};
use crate::fmt::{color_repo, link_str, print_indent};
use crate::util::{ask_yne, input, is_arch_repo, is_number_menu, NumberMenu, YesNoEdit};
use crate::{ai, info, print_error, printtr};

use ansiterm::{Color, Style};
use anyhow::{ensure, Context, Result};
use flate2::read::GzDecoder;
use indicatif::HumanBytes;
use raur::{Raur, SearchBy};
use regex::RegexSet;
use reqwest::get;
use srcinfo::Srcinfo;
use tr::tr;

#[derive(Debug)]
pub enum AnyPkg<'a> {
    RepoPkg(&'a alpm::Package),
    AurPkg(&'a raur::Package),
    Custom(&'a str, &'a Srcinfo, &'a srcinfo::Package),
}

pub async fn search(config: &Config) -> Result<i32> {
    let quiet = config.args.has_arg("q", "quiet");

    let repo_pkgs = search_repos(config, &config.targets)?;

    let targets = config
        .targets
        .iter()
        .map(|t| t.to_lowercase())
        .collect::<Vec<_>>();

    let custom_pkgs = search_pkgbuilds(config, &targets)?;

    let pkgs = search_aur(config, &targets)
        .await
        .context(tr!("aur search failed"))?;

    let print_custom = || {
        for (repo, srcinfo, pkg) in &custom_pkgs {
            let path = &config
                .pkgbuild_repos
                .repo(repo)
                .unwrap()
                .base(config, &srcinfo.base.pkgbase)
                .unwrap()
                .path;
            print_pkgbuild_pkg(config, repo, path, srcinfo, pkg, quiet);
        }
    };

    if config.sort_mode == SortMode::TopDown {
        for pkg in &repo_pkgs {
            print_alpm_pkg(config, pkg, quiet);
        }
        print_custom();
        for pkg in &pkgs {
            print_pkg(config, pkg, quiet)
        }
    } else {
        for pkg in pkgs.iter().rev() {
            print_pkg(config, pkg, quiet)
        }
        print_custom();
        for pkg in repo_pkgs.iter().rev() {
            print_alpm_pkg(config, pkg, quiet);
        }
    }

    Ok((repo_pkgs.is_empty() && pkgs.is_empty()) as i32)
}

fn search_pkgbuilds<'a>(
    config: &'a Config,
    targets: &[String],
) -> Result<Vec<(&'a str, &'a Srcinfo, &'a srcinfo::Package)>> {
    if !config.mode.pkgbuild() {
        return Ok(Vec::new());
    }

    let regex = RegexSet::new(targets)?;
    let mut ret = Vec::new();

    for repo in &config.pkgbuild_repos.repos {
        for base in repo.pkgs(config) {
            let base = &base.srcinfo;
            for pkg in &base.pkgs {
                if targets.is_empty()
                    || regex.is_match(&base.base.pkgbase)
                    || regex.is_match(&pkg.pkgname)
                    || pkg.pkgdesc.iter().any(|d| regex.is_match(d))
                    || pkg
                        .provides
                        .iter()
                        .flat_map(|p| p.values())
                        .any(|p| regex.is_match(p))
                    || pkg.groups.iter().any(|g| regex.is_match(g))
                {
                    ret.push((repo.name.as_str(), base, pkg))
                }
            }
        }
    }

    Ok(ret)
}

fn search_local<'a>(config: &'a Config, targets: &[String]) -> Result<Vec<&'a alpm::Package>> {
    let mut ret = Vec::new();

    if targets.is_empty() {
        ret.extend(config.alpm.localdb().pkgs());
    } else {
        let pkgs = config.alpm.localdb().search(targets.iter())?;
        ret.extend(pkgs);
    };

    if config.limit != 0 {
        ret.truncate(config.limit);
    }

    Ok(ret)
}

pub fn search_repos<'a>(config: &'a Config, targets: &[String]) -> Result<Vec<&'a alpm::Package>> {
    if targets.is_empty() || !config.mode.repo() {
        return Ok(Vec::new());
    }

    let mut ret = Vec::new();

    for db in config.alpm.syncdbs() {
        let pkgs = db.search(targets.iter())?;
        ret.extend(pkgs);
    }

    if config.limit != 0 {
        ret.truncate(config.limit);
    }

    Ok(ret)
}

async fn search_target(config: &Config, targets: &mut Vec<String>) -> Result<Vec<raur::Package>> {
    let by = config.search_by;
    let mut pkgs = Ok(Vec::new());
    let mut index = 0;

    for (i, target) in targets.iter().enumerate() {
        index = i;
        pkgs = config.raur.search_by(target, by).await;
        if !matches!(pkgs, Err(raur::Error::Aur(_))) {
            break;
        }
    }

    if pkgs.is_ok() {
        targets.remove(index);
    }

    Ok(pkgs?)
}

async fn search_aur_regex(config: &Config, targets: &[String]) -> Result<Vec<raur::Package>> {
    let url = config.aur_url.join("packages.gz")?;
    let resp = get(url.clone())
        .await
        .with_context(|| format!("get {}", url))?;
    let success = resp.status().is_success();
    ensure!(success, "get {}: {}", url, resp.status());

    let data = resp.bytes().await?;
    let decoder = GzDecoder::new(&*data);
    let data =
        std::io::read_to_string(decoder).with_context(|| tr!("failed to decode package list"))?;

    let regex = RegexSet::new(targets)?;

    let pkgs = data
        .lines()
        .filter(|pkg| regex.is_match(pkg))
        .collect::<Vec<_>>();
    ensure!(pkgs.len() < 2000, "too many packages");
    let pkgs = config.raur.info(&pkgs).await?;
    Ok(pkgs)
}

pub async fn search_aur(config: &Config, targets: &[String]) -> Result<Vec<raur::Package>> {
    if targets.is_empty() || !config.mode.aur() {
        return Ok(Vec::new());
    }

    let mut matches = if config.args.has_arg("x", "regex") {
        search_aur_regex(config, targets).await?
    } else {
        let mut targets = targets.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>();
        targets.sort_by_key(|t| t.len());

        let mut matches = Vec::new();

        let by = config.search_by;

        if by == SearchBy::NameDesc {
            let pkgs = search_target(config, &mut targets).await?;
            matches.extend(pkgs);
            matches.retain(|p| {
                let name = p.name.to_lowercase();
                let description = p
                    .description
                    .as_ref()
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default();
                targets
                    .iter()
                    .all(|t| name.contains(t) | description.contains(t))
            });
        } else if by == SearchBy::Name {
            let pkgs = search_target(config, &mut targets).await?;
            matches.extend(pkgs);
            matches.retain(|p| targets.iter().all(|t| p.name.to_lowercase().contains(t)));
        } else {
            for target in targets {
                let pkgs = config.raur.search_by(target, by).await?;
                matches.extend(pkgs);
            }
        }

        matches
    };

    match config.sort_by {
        SortBy::Votes => matches.sort_by_key(|p| std::cmp::Reverse(p.num_votes)),
        SortBy::Popularity => {
            matches.sort_by(|a, b| b.popularity.partial_cmp(&a.popularity).unwrap())
        }
        SortBy::Id => matches.sort_by_key(|p| p.id),
        SortBy::Name => matches.sort_by(|a, b| a.name.cmp(&b.name)),
        SortBy::Base => matches.sort_by(|a, b| a.package_base.cmp(&b.package_base)),
        SortBy::Submitted => matches.sort_by_key(|p| p.first_submitted),
        SortBy::Modified => matches.sort_by_key(|p| p.last_modified),
        _ => (),
    }

    if config.limit != 0 {
        matches.truncate(config.limit);
    }

    Ok(matches)
}

fn print_pkgbuild_pkg(
    config: &Config,
    repo: &str,
    path: &Path,
    srcinfo: &Srcinfo,
    pkg: &srcinfo::Package,
    quiet: bool,
) {
    if quiet {
        println!("{}", pkg.pkgname);
        return;
    }

    let c = config.color;

    let name = if let Some(url) = &pkg.url {
        link_str(c.enabled, &c.ss_name.paint(&pkg.pkgname), url)
    } else {
        c.ss_name.paint(&pkg.pkgname).to_string()
    };

    print!(
        "{}/{} {}",
        color_repo(c.enabled, repo),
        name,
        c.ss_ver.paint(srcinfo.version()),
    );

    if let Ok(repo_pkg) = config.alpm.localdb().pkg(&*pkg.pkgname) {
        let installed = if repo_pkg.version().as_str() != srcinfo.version() {
            tr!("[installed: {}]", repo_pkg.version())
        } else {
            tr!("[installed]")
        };

        print!(" {}", c.ss_installed.paint(installed));
    }

    let none = tr!("None");
    print!("\n    ");
    let desc = pkg.pkgdesc.as_deref().unwrap_or(&none).split_whitespace();
    print_indent(Style::new(), 4, 4, config.cols, " ", desc);

    if config.args.count("s", "search") > 1 {
        info::print(c, 14, config.cols, "    Path", &path.display().to_string());
    }
}

fn print_pkg(config: &Config, pkg: &raur::Package, quiet: bool) {
    if quiet {
        println!("{}", pkg.name);
        return;
    }

    let c = config.color;
    let stats = format!("+{} ~{:.2}", pkg.num_votes, pkg.popularity);

    let aur = color_repo(c.enabled, "aur");
    let aur = if let Ok(url) = config.aur_url.join(&format!("packages/{}", pkg.name)) {
        link_str(c.enabled, &aur, url.as_str())
    } else {
        aur
    };
    let name = if let Some(url) = &pkg.url {
        link_str(c.enabled, &c.ss_name.paint(&pkg.name), url)
    } else {
        c.ss_name.paint(&pkg.name).to_string()
    };
    print!(
        "{}/{} {} [{}]",
        color_repo(c.enabled, &aur),
        c.ss_name.paint(name),
        c.ss_ver.paint(&pkg.version),
        c.ss_stats.paint(stats),
    );

    if let Some(date) = pkg.out_of_date {
        let date = tr!("[out-of-date: {}]", crate::fmt::ymd(date));
        print!(" {}", c.ss_ood.paint(date));
    }

    if let Ok(repo_pkg) = config.alpm.localdb().pkg(&*pkg.name) {
        let installed = if repo_pkg.version().as_str() != pkg.version {
            tr!("[installed: {}]", repo_pkg.version())
        } else {
            tr!("[installed]")
        };

        print!(" {}", c.ss_installed.paint(installed));
    }

    if pkg.maintainer.is_none() {
        print!(" {}", c.ss_orphaned.paint(tr!("[orphaned]")));
    }

    let none = tr!("None");
    print!("\n    ");
    let desc = pkg
        .description
        .as_deref()
        .unwrap_or(&none)
        .split_whitespace();
    print_indent(Style::new(), 4, 4, config.cols, " ", desc);

    if config.args.count("s", "search") > 1 {
        if let Some(ref url) = pkg.url {
            info::print(c, 14, config.cols, "    URL", url);
        }

        let aur_url = format!("{}packages/{}", config.aur_url, pkg.package_base);
        info::print(c, 14, config.cols, "    AUR URL", aur_url.as_str());
    }
}

fn print_alpm_pkg(config: &Config, pkg: &alpm::Package, quiet: bool) {
    if quiet {
        println!("{}", pkg.name());
        return;
    }

    let c = config.color;
    let stats = format!(
        "{} {}",
        HumanBytes(pkg.download_size() as u64),
        HumanBytes(pkg.isize() as u64)
    );
    let ver: &str = pkg.version().as_ref();
    let mut repo = color_repo(c.enabled, pkg.db().unwrap().name());
    if is_arch_repo(pkg.db().unwrap().name()) {
        if let Ok(url) = config.arch_url.join(&format!(
            "packages/{}/{}/{}/",
            pkg.db().unwrap().name(),
            pkg.arch().unwrap_or("any"),
            pkg.name()
        )) {
            repo = link_str(c.enabled, &repo, url.as_str());
        }
    }

    let name = if let Some(url) = pkg.url() {
        link_str(c.enabled, &c.ss_name.paint(pkg.name()), url)
    } else {
        c.ss_name.paint(pkg.name()).to_string()
    };

    print!(
        "{}/{} {} [{}]",
        color_repo(c.enabled, &repo),
        c.ss_name.paint(name),
        c.ss_ver.paint(ver),
        c.ss_stats.paint(stats),
    );

    if let Ok(repo_pkg) = config.alpm.localdb().pkg(pkg.name()) {
        let installed = if repo_pkg.version() != pkg.version() {
            tr!("[installed: {}]", repo_pkg.version())
        } else {
            tr!("[installed]")
        };

        print!(" {}", c.ss_installed.paint(installed));
    }

    if !pkg.groups().is_empty() {
        print!(" {}", c.ss_orphaned.paint("("));
        print!("{}", c.ss_orphaned.paint(pkg.groups().first().unwrap()));
        for group in pkg.groups().iter().skip(1) {
            print!(" {}", c.ss_orphaned.paint(group));
        }
        print!("{}", c.ss_orphaned.paint(")"));
    }

    print!("\n    ");
    let desc = pkg.desc();
    let desc = desc.unwrap_or_default().split_whitespace();
    print_indent(Style::new(), 4, 4, config.cols, " ", desc);

    if config.args.count("s", "search") > 1 {
        if let Some(url) = pkg.url() {
            info::print(c, 14, config.cols, "    URL", url);
        }
    }
}

pub async fn interactive_search_local(config: &mut Config) -> Result<()> {
    let mut all_pkgs = Vec::new();
    let repo_pkgs = search_local(config, &config.targets)?;

    for pkg in repo_pkgs {
        all_pkgs.push(AnyPkg::RepoPkg(pkg));
    }

    let was_results = all_pkgs.is_empty();
    let targs = interactive_menu(config, all_pkgs, false).await?;
    if targs.is_empty() && !was_results {
        printtr!(" there is nothing to do");
    }
    config.targets = targs.clone();
    config.args.targets = targs;
    Ok(())
}

pub async fn interactive_search(config: &mut Config, install: bool) -> Result<()> {
    let repo_pkgs = search_repos(config, &config.targets)?;
    let custom_pkgs = search_pkgbuilds(config, &config.targets)?;
    let aur_pkgs = search_aur(config, &config.targets).await?;
    let mut all_pkgs = Vec::new();

    for pkg in repo_pkgs {
        all_pkgs.push(AnyPkg::RepoPkg(pkg));
    }
    for (repo, base, pkg) in custom_pkgs {
        all_pkgs.push(AnyPkg::Custom(repo, base, pkg));
    }
    for pkg in &aur_pkgs {
        all_pkgs.push(AnyPkg::AurPkg(pkg));
    }

    let was_results = all_pkgs.is_empty();
    let targs = interactive_menu(config, all_pkgs, install).await?;
    if targs.is_empty() && !was_results {
        printtr!(" there is nothing to do");
    }
    config.targets = targs.clone();
    config.args.targets = targs;
    Ok(())
}

pub async fn interactive_menu(
    config: &Config,
    mut all_pkgs: Vec<AnyPkg<'_>>,
    install: bool,
) -> Result<Vec<String>> {
    let pad = all_pkgs.len().to_string().len();

    if all_pkgs.is_empty() {
        // Nothing matched: hand the natural language to the ai which can search
        // the repositories itself instead of just returning empty handed.
        if config.ai {
            let query = config.targets.join(" ");
            return ai_discover(config, &query).await;
        }
        printtr!("no packages match search");
        return Ok(Vec::new());
    }

    let indexes = all_pkgs
        .iter()
        .enumerate()
        .filter_map(|(n, pkg)| {
            let name = match pkg {
                AnyPkg::RepoPkg(pkg) => pkg.name(),
                AnyPkg::AurPkg(pkg) => pkg.name.as_str(),
                AnyPkg::Custom(_, _, pkg) => pkg.pkgname.as_str(),
            };

            if config.targets.iter().any(|targ| targ == name) {
                Some(n)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    for (i, n) in indexes.iter().rev().enumerate() {
        let pkg = all_pkgs.remove(i + n);
        all_pkgs.insert(0, pkg);
    }

    if config.sort_mode == SortMode::TopDown {
        for (n, pkg) in all_pkgs.iter().enumerate() {
            print_any_pkg(config, n, pad, pkg)
        }
    } else {
        for (n, pkg) in all_pkgs.iter().enumerate().rev() {
            print_any_pkg(config, n, pad, pkg)
        }
    }

    let input = if install {
        input(config, &tr!("Packages to install (eg: 1 2 3, 1-3):"))
    } else {
        input(config, &tr!("Select packages (eg: 1 2 3, 1-3):"))
    };

    if input.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Anything the number menu understands is still handled by the number menu.
    // Only genuinely non numeric input is handed to the ai.
    if config.ai && !is_number_menu(&input, &menu_words(config, &all_pkgs)) {
        return ai_menu(config, &all_pkgs, input.trim(), install).await;
    }

    let menu = NumberMenu::new(&input);
    let mut pkgs = Vec::new();

    if config.sort_mode == SortMode::TopDown {
        for (n, pkg) in all_pkgs.iter().enumerate() {
            if menu.contains(n + 1, "") {
                pkgs.push(pkg_target(config, pkg));
            }
        }
    } else {
        for (n, pkg) in all_pkgs.iter().enumerate().rev() {
            if menu.contains(n + 1, "") {
                pkgs.push(pkg_target(config, pkg));
            }
        }
    }

    Ok(pkgs)
}

fn pkg_name<'a>(pkg: &'a AnyPkg<'_>) -> &'a str {
    match pkg {
        AnyPkg::RepoPkg(pkg) => pkg.name(),
        AnyPkg::AurPkg(pkg) => pkg.name.as_str(),
        AnyPkg::Custom(_, _, pkg) => pkg.pkgname.as_str(),
    }
}

fn pkg_repo<'a>(config: &'a Config, pkg: &'a AnyPkg<'_>) -> &'a str {
    match pkg {
        AnyPkg::RepoPkg(pkg) => pkg.db().unwrap().name(),
        AnyPkg::AurPkg(_) => config.aur_namespace(),
        AnyPkg::Custom(repo, _, _) => repo,
    }
}

fn pkg_target(config: &Config, pkg: &AnyPkg<'_>) -> String {
    format!("{}/{}", pkg_repo(config, pkg), pkg_name(pkg))
}

/// Every word the number menu could match, so prose can be told apart from a
/// selection by name.
fn menu_words<'a>(config: &'a Config, all_pkgs: &'a [AnyPkg<'_>]) -> Vec<&'a str> {
    let mut words = Vec::with_capacity(all_pkgs.len() * 2);

    for pkg in all_pkgs {
        words.push(pkg_name(pkg));
        words.push(pkg_repo(config, pkg));
    }

    words.sort_unstable();
    words.dedup();
    words
}

/// Hands a natural language selection to the ai.
///
/// The ai answers with numbers from the list that was just printed, which are
/// then resolved locally. It never names a package itself, so it cannot pull in
/// a target the user was not shown.
async fn ai_menu(
    config: &Config,
    all_pkgs: &[AnyPkg<'_>],
    query: &str,
    install: bool,
) -> Result<Vec<String>> {
    let c = config.color;
    let mut listing = String::new();

    for (n, pkg) in all_pkgs.iter().enumerate() {
        let (name, repo, version, desc, extra) = match pkg {
            AnyPkg::RepoPkg(pkg) => (
                pkg.name(),
                pkg.db().unwrap().name(),
                pkg.version().to_string(),
                pkg.desc().unwrap_or_default().to_string(),
                String::new(),
            ),
            AnyPkg::AurPkg(pkg) => (
                pkg.name.as_str(),
                "aur",
                pkg.version.clone(),
                pkg.description.clone().unwrap_or_default(),
                format!(" votes={} popularity={:.2}", pkg.num_votes, pkg.popularity),
            ),
            AnyPkg::Custom(repo, srcinfo, pkg) => (
                pkg.pkgname.as_str(),
                *repo,
                srcinfo.version(),
                pkg.pkgdesc.clone().unwrap_or_default(),
                String::new(),
            ),
        };

        let installed = if config.alpm.localdb().pkg(name).is_ok() {
            " installed"
        } else {
            ""
        };

        let _ = writeln!(
            listing,
            "{}. {}/{} {}{}{}\n   {}",
            n + 1,
            repo,
            name,
            version,
            extra,
            installed,
            desc
        );
    }

    // The conversation so far. Without this the model answers every follow up
    // as if it were the first question.
    let mut history: Vec<ai::Message> = Vec::new();
    let mut query = query.to_string();

    loop {
        let selection = match ai::select(config, &listing, &query, all_pkgs.len(), &history).await {
            Ok(selection) => selection,
            Err(err) => {
                print_error(c.error, err);
                return Ok(Vec::new());
            }
        };

        if !selection.reason.trim().is_empty() {
            ai::print_reason(config.cols, &selection.reason);
        }

        // Record this turn before asking, so a follow up carries it.
        history.push(ai::Message::user(query.clone()));
        history.push(ai::Message::assistant_text(ai::selection_json(&selection)));

        let names = selection
            .indices
            .iter()
            .map(|&n| pkg_name(&all_pkgs[n - 1]))
            .collect::<Vec<_>>()
            .join(" ");

        // With nothing to install there is no y to offer, only quit or keep talking.
        let answer = if selection.indices.is_empty() {
            ask_yne(config, &tr!("Nothing matched. Ask the ai again?"), false)
        } else if install {
            ask_yne(config, &tr!("Install {}?", names), true)
        } else {
            ask_yne(config, &tr!("Select {}?", names), true)
        };

        match answer {
            YesNoEdit::Yes if !selection.indices.is_empty() => {
                let mut pkgs = Vec::with_capacity(selection.indices.len());
                for &n in &selection.indices {
                    // Indices are validated against the list length before we get here.
                    pkgs.push(pkg_target(config, &all_pkgs[n - 1]));
                }
                return Ok(pkgs);
            }
            YesNoEdit::Edit => {
                let followup = input(config, &tr!("Tell the ai what to change:"));
                let followup = followup.trim();

                if followup.is_empty() {
                    if selection.indices.is_empty() {
                        printtr!("no packages match search");
                    }
                    return Ok(Vec::new());
                }

                query = followup.to_string();
            }
            _ => {
                if selection.indices.is_empty() {
                    printtr!("no packages match search");
                }
                return Ok(Vec::new());
            }
        }
    }
}

/// Resolves a candidate package name to (repo, desc).
async fn resolve_candidate(config: &Config, name: &str) -> Result<Option<(String, String)>> {
    // Exact lookups in the sync dbs first.
    if config.mode.repo() {
        for db in config.alpm.syncdbs() {
            if let Ok(pkg) = db.pkg(name) {
                let repo = db.name().to_string();
                let desc = pkg.desc().unwrap_or_default().to_string();
                return Ok(Some((repo, desc)));
            }
        }
    }

    // The AUR.
    if config.mode.aur() {
        let targets = vec![name.to_string()];
        if let Ok(pkgs) = search_aur(config, &targets).await {
            for pkg in pkgs {
                if pkg.name == name {
                    let desc = pkg.description.clone().unwrap_or_default();
                    return Ok(Some((config.aur_namespace().to_string(), desc)));
                }
            }
        }
    }

    Ok(None)
}

/// Asks the ai to find packages for a request that matched nothing, then lets
/// the user pick with a multi select.
///
/// The ai is free to search the repositories and the web; every name it returns
/// is resolved against the real databases before being offered, so a fabricated
/// package is dropped instead of installed.
///
/// If the request was not about packages at all (the user is just talking to
/// paru), the ai's reply is shown and the user may keep chatting or quit.
async fn ai_discover(config: &Config, query: &str) -> Result<Vec<String>> {
    let c = config.color;

    let mut executor = crate::ai_tools::ToolExecutor::new(config);
    let mut history: Vec<ai::Message> = Vec::new();
    let mut query = query.to_string();

    loop {
        let discovery = match ai::discover(config, &query, &mut history, &mut executor).await {
            Ok(discovery) => discovery,
            Err(err) => {
                print_error(c.error, err);
                return Ok(Vec::new());
            }
        };

        if !discovery.message.trim().is_empty() {
            ai::print_body(config.cols, &discovery.message);
            println!();
        }

        // Resolve to real packages, keeping the ai's reason alongside.
        let mut resolved = Vec::new();
        let mut candidates = discovery.candidates;
        candidates.dedup_by(|a, b| a.name == b.name);
        for candidate in candidates.drain(..) {
            if let Ok(Some((repo, desc))) = resolve_candidate(config, &candidate.name).await {
                resolved.push((candidate, repo, desc));
            }
        }

        if !resolved.is_empty() {
            return offer_candidates(config, resolved).await;
        }

        // Nothing to install: the model either chatted or found nothing. Offer
        // to keep talking so a greeting like "你好" becomes a conversation
        // instead of a dead end.
        let followup = input(config, &tr!("Ask the ai anything (enter to quit):"));
        let followup = followup.trim().to_string();
        if followup.is_empty() {
            return Ok(Vec::new());
        }
        query = followup;
    }
}

/// Offers a set of resolved candidates and returns the chosen targets.
async fn offer_candidates(
    config: &Config,
    resolved: Vec<(ai::Candidate, String, String)>,
) -> Result<Vec<String>> {
    let c = config.color;
    let pad = resolved.len().to_string().len();
    let namespace = config.aur_namespace();

    for (i, (cand, repo, desc)) in resolved.iter().enumerate() {
        // The internal namespace may be `__aur__` when the user has a repo
        // literally named `aur`; never show that implementation detail.
        let shown = if repo == namespace {
            "aur"
        } else {
            repo.as_str()
        };
        println!(
            "{} {}/{} - {}",
            c.number_menu.paint(format!("{:>pad$}", i + 1, pad = pad)),
            color_repo(c.enabled, shown),
            c.ss_name.paint(&cand.name),
            desc
        );
        print_indent(
            Style::from(Color::Fixed(240)),
            4,
            4,
            config.cols,
            " ",
            cand.reason.split_whitespace(),
        );
    }

    let selected = if config.no_confirm {
        (0..resolved.len()).collect::<Vec<_>>()
    } else {
        let answer = input(config, &tr!("Packages to install (eg: 1 2 3, 1-3):"));
        if answer.trim().is_empty() || answer.trim().eq_ignore_ascii_case("q") {
            Vec::new()
        } else {
            let menu = NumberMenu::new(&answer);
            (0..resolved.len())
                .filter(|&i| menu.contains(i + 1, ""))
                .collect::<Vec<_>>()
        }
    };

    if selected.is_empty() {
        return Ok(Vec::new());
    }

    Ok(selected
        .into_iter()
        .map(|i| {
            let (cand, repo, _) = &resolved[i];
            format!("{}/{}", repo, cand.name)
        })
        .collect())
}

fn print_any_pkg(config: &Config, n: usize, pad: usize, pkg: &AnyPkg) {
    let c = config.color;
    match pkg {
        AnyPkg::RepoPkg(pkg) => {
            let n = format!("{:>pad$}", n + 1, pad = pad);
            print!("{} ", c.number_menu.paint(n));
            print_alpm_pkg(config, pkg, false)
        }
        AnyPkg::AurPkg(pkg) => {
            let n = format!("{:>pad$}", n + 1, pad = pad);
            print!("{} ", c.number_menu.paint(n));
            print_pkg(config, pkg, false)
        }
        AnyPkg::Custom(repo, base, pkg) => {
            let n = format!("{:>pad$}", n + 1, pad = pad);
            print!("{} ", c.number_menu.paint(n));
            let path = &config
                .pkgbuild_repos
                .repo(repo)
                .unwrap()
                .base(config, &base.base.pkgbase)
                .unwrap()
                .path;
            print_pkgbuild_pkg(config, repo, path, base, pkg, false)
        }
    };
}
