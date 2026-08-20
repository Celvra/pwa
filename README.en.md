# Paru with AI

[中文](README.md)

Also known as PWA.

A fork of [paru](https://github.com/Morganamilo/paru), the AUR helper, with an optional AI layer. It keeps paru's familiar workflow while adding AI-assisted package search, PKGBUILD review, version comparison, and update risk assessment.

> [!WARNING]
> AI cannot prevent every malicious package or supply-chain attack. It can only try to understand PKGBUILDs, compare versions, and provide suggestions. Do not rely on this project to prevent system breakage or AUR poisoning. AI hallucinations are unavoidable, so review packages yourself and keep a rollback solution such as Timeshift ready.
>
> PWA does not take responsibility for system damage caused by AI-generated output. This is an experimental fork and created with AI. AI-assisted code is reviewed manually, but it may still contain bugs.

## Overview

`pwa` wraps pacman and supports searching, building, installing, and upgrading packages from the official repositories and the AUR. It keeps features such as PKGBUILD review, development-package tracking, and chroot builds while minimizing unnecessary interaction.

The AI layer is disabled by default. When enabled, it can use an OpenAI-compatible API to review PKGBUILDs, compare package versions, and assess potential system update risks. A Tavily API key additionally enables the AI web-search tool.

## Installation

```sh
sudo pacman -S --needed base-devel
git clone https://github.com/Celvra/pwa.git
cd pwa
makepkg -si
```

Verify the installation with:

```sh
pwa --help
```

## AI configuration

Edit `~/.config/paru/paru.conf` and add an `[ai]` section. PWA supports OpenAI-compatible APIs and local services such as Ollama:

```ini
[ai]
Url = https://your-api.example/v1
Model = your-model
KeyFile = ~/.config/paru/ai.key
```

The API key can also be provided through an environment variable:

```sh
export PARU_AI_KEY="your-api-key"
```

To enable AI web search, set `TavilyKey` in the configuration or export `PARU_TAVILY_KEY`.

AI output is provided for assistance and should not replace your own review. Always inspect PKGBUILDs and package upgrade plans before installing anything.

## Common operations

```sh
pwa <target>       # Interactively search for and install a package
pwa                # Equivalent to pwa -Syu; update the system
pwa -S <target>    # Install a specific package
pwa -Sua           # Upgrade AUR packages
pwa -Qua           # Show available AUR updates
pwa -G <target>    # Download a package's PKGBUILD and related files
pwa -Gp <target>   # Print a PKGBUILD
pwa -Gc <target>   # Print AUR comments
pwa --gendb        # Generate the database used to track -git packages
pwa -Bi .          # Build and install the PKGBUILD in the current directory
```

The command-line interface is largely compatible with `paru`.

### Using the AI layer

You can start an AI conversation with a natural-language query:

```sh
pwa <natural-language-query>
```

PWA can use AI to search for packages, search the web, filter search results, and provide package summaries in the language of your query. This can be useful when package descriptions are unclear.

After a package search reaches the numbered selection screen, you can enter a natural-language request. PWA provides the current result list to the AI, which can select a package and explain its choice. You can then decide whether to install it. Enter `e` to continue the conversation if the selected package is not what you wanted.

When configured, AI review may run automatically during:

- System updates
- PKGBUILD review
- AUR package updates

## Recommendations

- Color output follows pacman's configuration. Enable `Color` in `pacman.conf`.
- Enable `BottomUp` to display search results from the bottom upwards.
- Install [`bat`](https://github.com/sharkdp/bat) for syntax highlighting during PKGBUILD review.
- PWA tracks `-git` packages by monitoring their upstream repositories. Run `pwa --gendb` to register packages that were not installed by PWA.
- You can commit local PKGBUILD changes. When the package is updated, git will try to merge your changes with the upstream version.

For more options and configuration details, see [paru.8](./man/paru.8) and [paru.conf.5](./man/paru.conf.5).

> [!TIP]
> The documentation and configuration files still use paru's filenames. The executable is named `pwa`, so it can coexist with paru.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). The contribution guide has not been modified for this fork.

## Troubleshooting

PWA is not an official Arch Linux tool. If a package fails to build, first check whether `makepkg` can build it independently:

```sh
makepkg
```

If `makepkg` also fails, contact the package maintainer first. If only PWA fails, open an issue in this project and include the reproduction steps, commands, and relevant error output.
