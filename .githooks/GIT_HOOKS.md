# Git Hooks

本项目使用 Git Hooks 在提交和推送时自动执行代码质量检查，确保进入仓库的代码符合格式规范、无 Clippy 警告且通过所有测试。

## 包含的 Hooks

| Hook | 触发时机 | 执行内容 | 失败后果 |
|------|---------|---------|---------|
| `pre-commit` | `git commit` | `cargo fmt --check` | 阻止提交 |
| `pre-push` | `git push` | `cargo clippy -- -D warnings` + `cargo test` | 阻止推送 |

### pre-commit

提交前检查 Rust 代码格式。如果格式不符合 `rustfmt` 规范，提交会被阻止。

修复方法：

```bash
cargo fmt
```

然后重新提交即可。

### pre-push

推送前依次执行：

1. **Clippy 检查** — 以 `-D warnings` 模式运行，任何警告都会被视为错误
2. **测试** — 运行 `cargo test`，测试失败则阻止推送

## 快速安装

克隆项目后，运行以下命令激活 Hooks：

```bash
bash .githooks/setup-hooks.sh
```

该脚本会将 Git 的 `core.hooksPath` 指向 `.githooks/` 目录。

## 手动安装

如果不想使用脚本，也可以手动配置：

```bash
# 在项目根目录下执行
git config core.hooksPath .githooks
```

## 跳过 Hooks（不推荐）

在特殊情况下（如紧急热修复），可以通过 `--no-verify` 跳过 Hooks：

```bash
# 跳过 pre-commit
git commit --no-verify -m "hotfix: ..."

# 跳过 pre-push
git push --no-verify
```

> 注意：跳过 Hooks 可能导致不符合规范的代码进入仓库，仅建议在紧急情况下使用。CI 流水线会再次执行这些检查，本地跳过的检查在 CI 仍可能失败。

## 验证安装

安装后可以通过以下命令验证 Hooks 是否生效：

```bash
git config core.hooksPath
```

输出应为 `.githooks`。
