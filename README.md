# Fast Context Rust

[Fast Context MCP Server](https://github.com/awei84/fast-context-mcp) 的 Rust 实现。

## 前置条件

- Rust 工具链（`cargo`、`rustc`）
- Windsurf 账号 / API Key
- 系统已安装 `ripgrep`，并可通过 `rg` 访问，或通过 `FC_RG_PATH` 指定

安装 ripgrep：

```bash
# macOS
brew install ripgrep

# Debian / Ubuntu
sudo apt-get install ripgrep

# Fedora
sudo dnf install ripgrep

# Arch
sudo pacman -S ripgrep

# Windows
winget install BurntSushi.ripgrep.MSVC
# 或
choco install ripgrep
# 或
scoop install ripgrep
```

MCP Server 启动前会检查 `rg`。如果找不到 `rg`，进程会向 stderr 打印安装提示并以非 0 退出码退出。

## 构建

```bash
cd /workspace/mymcp/fast-context-rust
cargo build --release
```

二进制路径：

```bash
/workspace/mymcp/fast-context-rust/target/release/fast-context-rust
```

## Release 产物

生成独立二进制压缩包和 SHA-256 校验文件：

```bash
cd /workspace/mymcp/fast-context-rust
./scripts/build-release.sh
```

使用对应平台的命令校验 SHA-256：

```bash
# Linux
(cd dist && sha256sum -c *.sha256)

# macOS
(cd dist && shasum -a 256 -c *.sha256)
```

```powershell
# Windows PowerShell：将打印出的 hash 与 .sha256 文件内容对比
Get-FileHash .\fast-context-rust-<version>-windows-x64.zip -Algorithm SHA256
Get-Content .\fast-context-rust-<version>-windows-x64.zip.sha256
```

压缩包只包含 Rust 二进制和文档，不依赖 `npm`、`npx`、Node.js 或 `node_modules`。用户仍需安装系统 `ripgrep`，并确保可以通过 `rg` 访问，或用 `FC_RG_PATH` 指定路径。

手动安装时，解压压缩包，然后在 MCP 客户端中配置解压后二进制的绝对路径。

## MCP 客户端配置

将 command 配置为二进制绝对路径。例如：

```json
{
  "mcpServers": {
    "fast-context-rust": {
      "command": "/workspace/mymcp/fast-context-rust/target/release/fast-context-rust",
      "env": {
        "WINDSURF_API_KEY": "your-windsurf-api-key"
      }
    }
  }
}
```

如果 MCP 客户端的 PATH 中没有 `rg`，请设置 `FC_RG_PATH`：

```json
{
  "env": {
    "WINDSURF_API_KEY": "your-windsurf-api-key",
    "FC_RG_PATH": "/usr/bin/rg"
  }
}
```

## MCP 工具

Rust MCP Server 只暴露一个工具：

- `fast_context_search`

API Key 提取能力**不是 MCP tool**。

### 工具参数

从 1.4.0 起，`fast_context_search` 的 schema 只暴露 7 个任务级参数。服务端策略（每轮最大命令数、网络超时、repo-map 模式）通过 `FC_*` 环境变量在 server 启动时配置，每次 MCP 会话常驻的 schema token 开销更小。

| 参数                    | 类型     | 必填   | 默认值  | 说明                                                                     |
| ----------------------- | -------- | ------ | ------- | ------------------------------------------------------------------------ |
| `query`                 | string   | 是     | —       | 自然语言搜索查询                                                         |
| `project_path`          | string   | **是** | —       | 项目根目录的绝对路径                                                     |
| `tree_depth`            | integer  | 否     | `3`     | 仓库目录树深度（0-6，0 = 自动）。超过 250KB 会自动降级                   |
| `max_turns`             | integer  | 否     | `3`     | 搜索轮数（1-5）。越多越深入，但越慢                                      |
| `max_results`           | integer  | 否     | `10`    | 最多返回的文件数（1-30）                                                 |
| `exclude_paths`         | string[] | 否     | `[]`    | 从 repo map 和搜索上下文中排除的目录/文件模式                            |
| `include_code_snippets` | boolean  | 否     | `false` | 是否在返回结果中包含代码片段。默认只返回文件路径、行号范围和 grep 关键词 |

### 环境变量

| 变量名                | 默认值               | 说明                                                  |
| --------------------- | -------------------- | ----------------------------------------------------- |
| `WINDSURF_API_KEY`    | —                    | 真实语义搜索所需的 Windsurf API Key                   |
| `FC_RG_PATH`          | —                    | `rg` 可执行文件的绝对路径（覆盖 PATH 查找）           |
| `FC_MAX_TURNS`        | `3`                  | 调用方未传 `max_turns` 时的默认值（1-5）              |
| `FC_MAX_RESULTS`      | `10`                 | 调用方未传 `max_results` 时的默认值（1-30）           |
| `FC_TREE_DEPTH`       | `3`                  | 调用方未传 `tree_depth` 时的默认值（0-6）             |
| `FC_MAX_COMMANDS`     | `8`                  | 远端 AI 每轮最多发出的命令数（1-20）                  |
| `FC_TIMEOUT_MS`       | `30000`              | 流式请求连接超时毫秒数（1000-300000）                 |
| `FC_RESULT_MAX_LINES` | `50`                 | 每条命令输出的最大行数（截断，1-500）                 |
| `FC_LINE_MAX_CHARS`   | `250`                | 每行输出的最大字符数（截断，20-10000）                |
| `FC_INCLUDE_SNIPPETS` | `false`              | 是否默认随搜索结果返回代码片段                        |
| `FC_REPO_MAP_MODE`    | `bootstrap_hotspot`  | Repo map 构建策略（`bootstrap_hotspot` 或 `classic`） |
| `WS_MODEL`            | `MODEL_SWE_1_6_FAST` | Windsurf 模型名称                                     |
| `WS_APP_VER`          | `1.48.2`             | Windsurf 应用版本（协议元数据）                       |
| `WS_LS_VER`           | `1.9544.35`          | Windsurf 语言服务器版本（协议元数据）                 |

## 提取 Windsurf API Key

通过 CLI 模式运行。该模式只输出 API Key，然后退出，不启动 MCP Server。

```bash
# 使用默认 Windsurf state.vscdb 位置
/workspace/mymcp/fast-context-rust/target/release/fast-context-rust --extract-windsurf-key

# 使用显式 SQLite 数据库路径
/workspace/mymcp/fast-context-rust/target/release/fast-context-rust --extract-windsurf-key --db-path /path/to/state.vscdb
```

默认数据库位置：

| 平台    | 路径                                                                    |
| ------- | ----------------------------------------------------------------------- |
| macOS   | `~/Library/Application Support/Windsurf/User/globalStorage/state.vscdb` |
| Windows | `%APPDATA%/Windsurf/User/globalStorage/state.vscdb`                     |
| Linux   | `${XDG_CONFIG_HOME:-~/.config}/Windsurf/User/globalStorage/state.vscdb` |

## 验证

```bash
cargo fmt --check
cargo test
cargo clippy -- -D warnings

# 检查 ripgrep 前置条件
cargo run -- --check-rg
```
