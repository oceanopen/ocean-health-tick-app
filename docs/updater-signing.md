# 自动更新与签名配置

本文档描述 Ocean Health Tick 桌面应用的自动更新（updater）链路：签名密钥管理、`tauri.conf.json` 配置、GitHub Actions 发布工作流，以及完整的发版流程。

## 整体链路

```
pnpm release (bumpp 改版本号 + 打 tag)
        │
        ▼
push tag v* ──► 触发两个 workflow
        │
        ├─► release-assets.yml  (mac universal + windows)
        │       tauri-action 用 TAURI_SIGNING_PRIVATE_KEY 给产物签名
        │       生成 *.sig + latest.json，全部上传到 GitHub Release
        │       └─► sync-gitee job（调用 release-gitee-sync.yml 可复用 workflow，矩阵完成后 best-effort 执行）
        │             · commit+tag 镜像到 Gitee（hub-mirror-action）
        │             · 构建资产上传到 Gitee Release（attach_files）
        │             · 改写出「下载 URL 指向 Gitee」的 latest-gitee.json
        │             · 双推到 release-manifest orphan 分支（GitHub + Gitee）
        │
        └─► release-notes.yml   (ubuntu)
                changelogithub 根据 conventional commits 生成 Release Notes

客户端检查更新（双源回退，GitHub 优先）：
  plugin 按 endpoints 数组顺序串行尝试，单源 5s 超时：
    1. GET github .../releases/latest/download/latest.json        （主源）
    2. GET gitee  .../raw/release-manifest/latest-gitee.json      （兜底）
  命中任一 → 比对版本 → 下载该源返回的 URL → pubkey 校验 *.sig → 安装
  两端都失败 → 正常展示报错信息
```

关键点：
- **latest.json 必须由 tauri-action 在 release 阶段生成**，因为它要聚合当次产物的签名。手动上传会漏掉它，自动更新就会失效。
- **Gitee 专属 `latest-gitee.json` 由 sync-gitee job 改写生成**（把 GitHub 下载 URL 换成 Gitee Release 附件 URL），签名/版本号等字段沿用 GitHub 那份（同一把私钥签名，验签公钥一致）。Gitee 没有 `releases/latest/download/` 静态快捷方式，故托管在 `release-manifest` orphan 分支走 raw URL。
- **客户端双源配置存在「鸡生蛋」**：仅首个含双源 endpoints 的版本（即合入此改动后的下次 release）及之后构建出的客户端才具备 Gitee 兜底；之前的已发布版本仍是单源 GitHub。该版本发版时 CI 会同步填充 Gitee，故新版本用户的 Gitee 兜底即时可用。

---

## 一、签名密钥

### 1. 生成密钥对（一次性）

```bash
pnpm tauri signer generate -w ~/.tauri/ocean-health-tick.key
```

输出示例：

```
私钥:  ~/.tauri/ocean-health-tick.key
公钥:  ~/.tauri/ocean-health-tick.key.pub
```

- **私钥文件**：本地妥善保存，绝不入库；CI 构建时通过 Secret 注入。
- **公钥**：写入 `tauri.conf.json`，随客户端一起分发，用于校验更新包。

> 若生成时设置了密码，CI 还需配置 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。本项目当前密钥未设密码。

### 2. 配置公钥到 tauri.conf.json

`src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`：

```jsonc
{
  // ...
  "updater": {
    "active": true,
    // 双源回退：plugin 按数组顺序串行尝试，GitHub 在前为主源、Gitee 在后为兜底。
    // 单源 5s 超时（check({ timeout: 5_000 })），最坏合计 10s。
    "endpoints": [
      "https://github.com/oceanopen/ocean-health-tick-app/releases/latest/download/latest.json",
      "https://gitee.com/ocean-open/ocean-health-tick-app/raw/release-manifest/latest-gitee.json"
    ],
    "pubkey": "公钥Base64",
    "windows": {
      "installMode": "passive"
    }
  }
  // ...
}
```

> 注意 owner 拼写差异：GitHub 是 `oceanopen`（无连字符），Gitee 是 `ocean-open`（有连字符）。

同时 `bundle.createUpdaterArtifacts: true` 让 bundler 额外产出 updater 产物（自动更新必需），与 `targets: ["app", "dmg", "nsis"]` 共存。其中 `app` 是 macOS 上唯一 updater-enabled 的 target（产出 `.app.tar.gz` + `.sig`），`dmg` 只是首次安装包、不会产 updater 产物；`nsis` 在 Windows 上产出 `.exe` + `.exe.sig`。

### 3. 私钥配置到 GitHub Secret

仓库 **Settings → Secrets and variables → Actions → New repository secret**：

| Name                                 | Value                                        |
| ------------------------------------ | -------------------------------------------- |
| `TAURI_SIGNING_PRIVATE_KEY`          | `cat ~/.tauri/ocean-health-tick.key` 的完整内容 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | （可选）密钥密码，未设密码则不需要           |

> `GITHUB_TOKEN` 由 Actions 自动注入，无需手动配置。

#### Gitee 镜像同步 secrets（sync-gitee job 用）

为国内访问受限提供 Gitee 兜底源，需额外配置 3 个 secret（与 [we_share_codes](https://github.com/oceanopen/we_share_codes) 同名复用）：

| Name                 | Value                                                                           |
| -------------------- | ------------------------------------------------------------------------------- |
| `GITEE_USER`         | `ocean-open`（注意有连字符；Gitee 组织名）                                       |
| `GITEE_TOKEN`        | Gitee 个人访问令牌（scope: `projects` + `user_info`），用于创建 Release、上传资产、推 orphan 分支 |
| `GITEE_PRIVATE_KEY`  | SSH 私钥（本地生成的 SSH keypair 的私钥），hub-mirror-action 用；公钥需配到 Gitee 组织 SSH 设置（标题 `hub-mirror`） |

前置一次性准备：

1. Gitee 组织 `ocean-open` 下**手动创建空仓库** `ocean-health-tick-app`（hub-mirror-action 跨组织不自动建仓）。
2. Gitee 组织设置 → SSH 公钥，添加与 `GITEE_PRIVATE_KEY` 配对的公钥。
3. 生成 Gitee token 并配到 GitHub Secrets。

> sync-gitee job 在 secrets 缺失时（如 fork 仓库）自动跳过，不会让 CI 报红。

---

## 二、GitHub Actions 工作流

### 1. `release-assets.yml` — 构建 + 签名 + 发布

`.github/workflows/release-assets.yml`：

- **触发**：push 匹配 `v*` 的 tag，或 Actions 页面手动触发。
- **矩阵**：`macos-latest`（universal：x86_64 + aarch64）/ `windows-latest`。
- **关键步骤**：`tauri-apps/tauri-action@v0`，仅当 `github.ref_type == 'tag'` 时执行 release：
  - 注入 `TAURI_SIGNING_PRIVATE_KEY` → 给 macOS `.app.tar.gz` / Windows `.exe` 签名生成 `.sig`
  - 生成 `latest.json` manifest（含版本、下载 URL、签名）
  - 上传 dmg / exe / `.app.tar.gz` / `*.sig` / `latest.json` 到 GitHub Release
- `updaterJsonPreferNsis: true`：Windows 在 `latest.json` 里走 nsis 的 `.exe` 路径而非 msi。

### 2. `release-notes.yml` — Release Notes 生成

`.github/workflows/release-notes.yml`：

- **触发**：push 匹配 `v*` 的 tag。
- **关键步骤**：`pnpx changelogithub` 根据 conventional commits 自动生成更新日志并创建 Release。

### 3. 可复用 workflow `release-gitee-sync.yml` — Gitee 镜像同步

抽离为独立文件 `.github/workflows/release-gitee-sync.yml`（`on: workflow_call`），由 `release-assets.yml` 在 `build` 矩阵全部成功后经 `needs: build` + `uses: ./.github/workflows/release-gitee-sync.yml` + `secrets: inherit` 调用。`github.ref_name` / `github.sha` / 所有 secrets 通过调用上下文自动继承，故被调用 workflow 无需 inputs。best-effort（secrets 缺失自动跳过，绝不阻塞 GitHub 主流程）。7 步：

1. **Gate**：检查 `GITEE_TOKEN` / `GITEE_USER` / `GITEE_PRIVATE_KEY` 齐全性。
2. **镜像 commit+tag**：`Yikun/hub-mirror-action@master`（`force_update` 强推含 tags）。
3. **下载 GitHub Release 资产**：`gh release download`。
4. **创建/复用 Gitee Release**：python3，`access_token` 放 body（Gitee v5 鉴权坑）。
5. **上传资产到 Gitee Release**：`attach_files`（curl multipart，上传前去重避免 re-run 重复，跳过 GitHub 版 `latest.json`）。
6. **改写 manifest**：查 Gitee API 拿真实 `browser_download_url` → 把 `latest.json` 的 `platforms.*.url` 换成 Gitee URL → 输出 `latest-gitee.json`（签名/版本号不变）。
7. **双推 orphan 分支**：把 `latest-gitee.json` force-push 到 `release-manifest` 分支，**同时推 GitHub 和 Gitee**（Gitee 端即时可用；GitHub 端留存审计，且对镜像 ref 处理策略稳健）。

> Gitee 没有 `releases/latest/download/{file}` 静态快捷方式，故 manifest 托管在 `release-manifest` 分支走 raw URL：`https://gitee.com/ocean-open/ocean-health-tick-app/raw/release-manifest/latest-gitee.json`。raw 有 CDN 缓存延迟（~1 分钟），但 Gitee 仅作兜底、不依赖时效性。

---

## 三、发版流程

本项目用 [`bumpp`](https://github.com/antfu/bumpp)（`pnpm release`）统一改版本号：

```bash
pnpm release
```

`bump.config.ts` 会同步更新：

- `package.json`
- `src-tauri/Cargo.toml`
- `src-tauri/tauri.conf.json`
- `src-tauri/Cargo.lock`（通过自定义 `execute` 钩子）

bumpp 交互式选择 patch/minor/major 后，会自动：

1. 提交版本号变更 commit
2. 打 `v<x.y.z>` tag
3. push 到远端

push tag 后，两个 workflow 并行触发，约 10–15 分钟完成构建、签名和发布。

---

## 四、验证发布结果

发版完成后，到 Release 页面确认以下产物齐全：

- `Ocean Health Tick_<ver>_universal.dmg` / `.app.tar.gz` / `.app.tar.gz.sig`（macOS）
- `Ocean Health Tick_<ver>_x64-setup.exe` / `.exe.sig`（Windows）
- `latest.json`

直接访问 endpoints URL 验证 manifest 可达：

```
https://github.com/oceanopen/ocean-health-tick-app/releases/latest/download/latest.json
https://gitee.com/ocean-open/ocean-health-tick-app/raw/release-manifest/latest-gitee.json
```

返回的 JSON 应包含当次版本号、各平台下载 URL 和 `signature` 字段。两份 manifest 的 `version` / `signature` 必须一致；Gitee 那份的 `platforms.*.url` 应全部指向 `gitee.com/.../releases/download/...`（无 github URL 残留）。

Gitee 侧额外确认：`ocean-open/ocean-health-tick-app` 仓库存在对应 tag、Release 含全部二进制附件、`release-manifest` 分支含 `latest-gitee.json`。

---

## 五、故障排查

| 现象                                       | 可能原因                                                             |
| ------------------------------------------ | -------------------------------------------------------------------- |
| CI 报 `TAURI_SIGNING_PRIVATE_KEY` 相关错误 | Secret 未配置 / 值不完整（含尾部换行）/ 设了密码但未配 `*_PASSWORD`  |
| Release 没有 `latest.json`                 | workflow 触发时不是 tag，或 tauri-action 步骤被跳过                  |
| 客户端提示签名校验失败                     | 公钥与私钥不匹配，或产物被手动重新打包覆盖了签名                     |
| 客户端找不到更新                           | endpoints URL 与实际 Release 仓库不一致；或 latest.json 中版本号未升 |
| Windows 更新走错包格式                     | `updaterJsonPreferNsis` 与 `bundle.targets` 不一致                   |
| sync-gitee job 整个被跳过                  | `GITEE_TOKEN` / `GITEE_USER` / `GITEE_PRIVATE_KEY` 任一未配置（Gate 步骤 skip） |
| 创建 Gitee Release 报 HTML 错误页          | 把 `access_token` 放进了 header；必须放 body / form field（Gitee v5 坑） |
| `latest-gitee.json` 404 或内容滞后         | `release-manifest` 分支未推送成功；或 Gitee raw CDN 缓存（等 ~1 分钟）；或该分支被 hub-mirror `--mirror` 删除（检查是否双推） |
| Gitee Release 附件重复                     | sync-gitee job 重跑时 attach_files 同名会追加（不覆盖）；手动清理即可 |
