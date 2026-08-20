# Paru with AI

[English](README.en.md)

简称 PWA.

基于 [paru](https://github.com/Morganamilo/paru) 的 AUR 助手 fork 版本，在保留 paru 常用工作流的基础上，加入了可选的 AI 功能层，以让 AI 检查，避免滚挂、AUR 投毒包。

> [!WARNING]
> AI 无法避免所有投毒操作，因此只能尽量让 AI 理解 PKGBUILD 的意思并提供建议，以及让 AI 进行版本比较。请勿过度依赖本项目的 AI 功能，其并不能解决一切; AI 幻觉是无可避免的，您仍需要做好滚挂的准备并准备类似 Timeshift 的回滚工具。
> PWA 不对 AI 产生的幻觉导致的系统损坏等负责，请谨慎对待。
> 这是一个 Vibe 项目，因此我会尽力 Review 一遍 Vibe 的代码。

## 项目简介

`pwa` 是一个基于 `paru` 的 pacman 封装工具，支持从官方仓库和 AUR 搜索、构建、安装及升级软件包。它尽量减少不必要的交互，同时保留 PKGBUILD 审查、开发版软件包跟踪、chroot 构建等功能。

AI 功能默认关闭。启用后，可使用 OpenAI 兼容 API 辅助检查待升级的软件包和潜在的系统更新风险、以及自动审查 PKGBUILD；配置 Tavily API 后，还可以使用 AI 网页搜索工具。

## 安装

```sh
sudo pacman -S --needed base-devel
git clone https://github.com/Celvra/pwa.git
cd pwa
makepkg -si
```

安装完成后可用以下命令确认可用性：`pwa --help`

## AI 配置

编辑 `~/.config/paru/paru.conf`，添加一个 `[ai]` 配置段。pwa 支持 OpenAI 兼容的 API，也可以连接本地服务例如 Ollama：

```ini
[ai]
Url = https://urapi.example/v1
Model = ur-model
KeyFile = ~/.config/paru/ai.key
```

也可以通过环境变量提供密钥：

```sh
export PARU_AI_KEY="your-api-key"
```

如需 AI 网页搜索，在配置中添加 `TavilyKey`，或设置 `PARU_TAVILY_KEY` 环境变量。请注意，AI 输出仅供参考和辅助理解，安装前仍应自行审查 PKGBUILD 和软件包升级计划，不要过度依赖 AI，AI 可能产生幻觉。PWA 不对 AI 产生的幻觉导致的系统损坏等负责，请谨慎对待，

## 常用操作

```sh
pwa <目标>       # 交互式搜索并安装目标软件包/在搜索失败时进入 AI 对话
pwa              # 等价于 pwa -Syu，系统更新
pwa -S <目标>    # 安装指定软件包
pwa -Sua         # 升级 AUR 软件包
pwa -Qua         # 查看可用的 AUR 更新
pwa -G <目标>    # 下载目标软件包的 PKGBUILD 及相关文件
pwa -Gp <目标>   # 输出 PKGBUILD
pwa -Gc <目标>   # 输出 AUR 评论
pwa --gendb      # 生成用于跟踪 -git 软件包的开发版数据库
pwa -Bi .        # 构建并安装当前目录中的 PKGBUILD
```

基本与 `paru` 一致。

### AI 层使用

你可以通过 `pwa <自然语言>` 与 AI 进行对话。你可以通过自然语言让 AI 搜索包、搜索网络，并筛选合适的软件包。并提供符合你语言的包简介，非常适合在提供的详情模糊的情况下搜索符合要求的包。

你可以在已搜索的情况下，即进入数字筛选界面时使用自然语言询问 AI, pwa 会向 AI 提供当前列表中的内容方便 AI 做出选择，在此模式下，AI 将会提供一个精确的包并给予理由，直接询问你是否安装。你也可以通过输入 `e` 来进入下一轮对话以避免当前包不符合你的意愿

若你已经配置 AI 配置，那么 pwa 理应会在：

- 系统更新时

- 审查 PKGBUILD 时

- AUR 包更新时

自动启动 AI Review。

## 使用建议

- 颜色输出取决于 pacman 的配置，请在 `pacman.conf` 中启用 `Color`。
- 启用 `BottomUp` 后，搜索结果会从底部向上显示。（推荐）
- 审查 PKGBUILD 时安装 [`bat`](https://github.com/sharkdp/bat) 可获得语法高亮。
- pwa 通过监控上游仓库跟踪 `-git` 软件包；对于并非由 pwa 安装的软件包，可运行 `pwa --gendb` 建立记录。
- 修改 PKGBUILD 后可以提交到本地 git 仓库。软件包更新时，git 会尝试合并上游的修改。

更多选项和配置说明请参阅 [paru.8](./man/paru.8) 与 [paru.conf.5](./man/paru.conf.5)。

> [!TIP]
> 当前文档和配置文件仍沿用 paru 的文件名，命令行程序名为 `pwa`，可与 paru 共存，避免日常操作体验不佳。

## 参与贡献

请参阅 [CONTRIBUTING.md](./CONTRIBUTING.md)，未作修改。

## 问题排查

pwa 不是 Arch Linux 官方工具。如果软件包构建失败，请先确认 `makepkg` 能否独立完成构建：

```sh
makepkg
```

如果 `makepkg` 也失败，应先联系软件包维护者；如果只有 pwa 失败，请在本项目提交问题，并附上复现步骤、相关命令和错误输出。
