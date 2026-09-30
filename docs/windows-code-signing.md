# Windows 代码签名（Authenticode）配置

本文档描述 Windows 产物的 Authenticode 代码签名链路：为什么需要签名、SignPath 签名管道设计、GitHub Secrets 配置，以及零成本的自签测试证书验证流程。

## 背景：为什么需要 Windows 代码签名

CI 构建的 Windows 安装包（`WeHealthTick-<version>-windows-x64-setup.exe`）**未做 Authenticode 签名**时，曾被公司安全软件以「木马 / Hijacker 家族 / 引擎检出 1/28」告警。经排查为**低置信度启发式误报**，触发因素叠加：

| # | 因素 | 说明 |
| --- | --- | --- |
| 1 | 无代码签名（主因） | 发行者无签名、无信誉积累，启发式引擎信任分极低，任何行为特征都会被放大 |
| 2 | NSIS 安装器格式 | 恶意软件常用分发格式，部分引擎对无签名 NSIS 包有先验加权 |
| 3 | 行为画像撞上 Hijacker 定义 | 健康提醒工具天然带：开机自启（写注册表 `Run` 键）、自动更新（下载并静默安装）、托盘常驻、currentUser 模式释放 exe 到用户目录 |
| 4 | 低信誉分发渠道 | GitHub Release 直链，每个新版本哈希对引擎都是全新未知文件 |

「1/28 引擎检出 + 泛家族名标签」是典型误报特征（真实恶意样本通常命中 10+ 引擎）；源码公开、CI 可审计亦可佐证。另经代码查证：沙箱引爆画像本就干净（自启动 opt-in、更新检查纯手动），判定更可能来自静态特征（无签名 + NSIS 壳 + 零信誉）。但根治手段就是给产物做 Authenticode 签名：绝大多数引擎对有效签名直接放行，签名信誉积累后误报趋近于零。

> 如需向公司安全团队说明，可直接引用本节与公开仓库地址（开源可审计是最佳清白证明）。

## 两套签名机制（勿混淆）

| | updater 签名（minisign） | Windows 代码签名（Authenticode） |
| --- | --- | --- |
| 目的 | 客户端校验更新包完整性 | 向系统/杀软证明发行者身份 |
| 密钥 | `TAURI_SIGNING_PRIVATE_KEY` | 证书（本文档） |
| 产物 | `*.sig` 文件 + `latest.json` | 二进制内嵌签名 |
| 配置 | `plugins.updater.pubkey` | `bundle.windows.signCommand` |
| 详见 | [updater-signing.md](./updater-signing.md) | 本文档 |

两者相互独立，都已启用时 Windows 产物同时携带两类签名。

## 签名管道架构

```
CI (release-assets.yml, windows-latest 矩阵)
  args: --config src-tauri/tauri.windows.conf.json   ← Windows 专属 override（与 tauri.dev.conf.json 场景 override 同模式）
        │
        ▼ 深层合并进主配置
  bundle.windows.signCommand = { cmd: "pwsh", args: [..., "src-tauri/scripts/sign.ps1", "%1"] }
        │
        ▼ tauri bundler 对每个 Windows 待签产物逐个调用
  （主程序 exe → NSIS 安装器 → NSIS 卸载器；%1 替换为产物绝对路径，
    相对路径参数由 bundler 相对构建 cwd 转绝对，卸载器签名 hook 在别的目录也成立）
        │
        ▼
  src-tauri/scripts/sign.ps1 按凭证两路分派：
    1. SIGNPATH_API_TOKEN → SignPath REST API（SubmitWithArtifact 上传 → 轮询
       Status 至终态 → 下载 SignedArtifact 回写原路径；证书与策略绑定全在门户侧）
    2. 未配置 → 醒目跳过日志 + exit 0（构建照常完成，CI 不变红）
```

关键设计：

- **平台 override 注入**：`signCommand` 只存在于 `tauri.windows.conf.json`（Windows 平台轴命名；`tauri.dev.conf.json` 为场景轴，同属按需 `--config` 合并的 override 配置）。CI 构建与本地 Windows 手动签名经 `--config` 合并生效；不传该配置的本地 `pnpm tauri build` 完全不受影响。
- **secrets 未配置不阻塞**：`SIGNPATH_API_TOKEN` 为空时脚本走显式跳过分支，CI 行为与引入签名前完全一致。**token 填入 GitHub Secrets 后，下次推 tag 自动生效，无需改任何代码；换正式证书也只改一个策略 slug secret（见下文）。**
- **macOS 矩阵不传该配置**：`bundle.windows` 为 Windows 专属配置，macOS 构建无需注入。

## 证书渠道

**决策（2026-09）**：唯一启用渠道为 **SignPath Foundation**——免费、完全 CI 化（无需本地机器）、无个人身份验证、私钥存其 HSM、证书主体面向项目。操作流程见下节。

> 更早的完整备选调研（SSL.com eSigner / Certum 开源证书 / Azure Trusted Signing / DigiCert KeyLocker / 国内 CA 的价格、流程、限制与回落顺序）在 2026-09-30 文档精简时删除，需要时从本文件的 git 历史恢复或重新调研。结论要点存档：Azure 排除系其个人身份验证仅限美/加等地区；Certum 最便宜但云签名无官方无头 CI 路径，需自托管 Windows 构建机。

## SignPath Foundation 操作流程

[signpath.org](https://signpath.org/)（签名平台 [signpath.io](https://signpath.io/)）是面向开源项目的免费代码签名基金。关键事实：

- **免费**："For OSS projects, our services are free of charge"。
- **无需个人身份验证**：验证的是**构建与仓库的关联**而非人，绕开护照 + 地址证明流程（对国内个人是最大摩擦点），亦无地区限制表述。
- **私钥不出 HSM**：证书私钥在其 FIPS 级 HSM 内生成与存储，签名在云端完成——无 token 邮寄、无本地机器，**CI 侧永不接触证书材料**。
- **证书主体面向项目**：签名的含义是「该二进制确实构建自你的开源仓库」，比个人证书更强的可审计性——对杀软信誉和向公司安全团队说明误报均有利。

2026-09 申请已批准。门户对象（[项目页](https://app.signpath.io/Web/c73af081-eb67-45b4-b569-fc1544b35c21/Projects/fc29e2b8-751c-482e-8a96-85222dd8bb8f)）：

| 门户对象 | 值 |
| --- | --- |
| Organization | Ocean Open，ID `c73af081-eb67-45b4-b569-fc1544b35c21` |
| Project | We Health Tick，slug `We_Health_Tick`（API 用 slug，非 URL 里的 GUID） |
| 测试证书 | `We Health Tick Test`（自签，软件密钥库——HSM 在当前订阅不可用，测试用途官方也只建议软件库） |
| CI 用户 | `GitHub Actions CI`（挂 API token，授权本项目 Submitter） |
| 测试策略 | `test-signing`（引用测试证书；Submitters 含 CI 用户；审批/可信构建/origin 三个限制**全不勾** → REST API 直提、无人值守） |

### REST API 集成（已实现）

策略详情页给出的 API 片段证实集成形态为 **REST API 直提**（非 GitHub App 强制模式）。落点即 sign.ps1（signCommand 调用链内，保证 updater minisign `.sig` 对签名后产物计算的顺序，不能事后补签）：

```
POST https://app.signpath.io/Api/v1/{orgId}/SigningRequests/SubmitWithArtifact
     multipart: projectSlug / signingPolicySlug / artifact(+description) → 201 + Location 头
GET  {Location}/Status         → 轮询至终态（Completed / Failed / Denied / Canceled）
GET  {Location}/SignedArtifact → 下载 → 临时文件 → 原子替换原路径
```

门户片段里的 `Submit-SigningRequest` PowerShell 封装未采用——上述三端点内联进 sign.ps1（约 50 行），避免外部模块依赖，语义一致。CI 侧凭证只有 `SIGNPATH_API_TOKEN`，配置见下节。

### 换正式证书（Foundation 把 release 证书挂进 org 后）

1. 门户：为正式证书建 `release-signing` 策略（Submitters 加 CI 用户；三个限制按 Foundation 要求勾选——**这是唯一变量**，见下）；
2. GitHub Secrets：`SIGNPATH_SIGNING_POLICY_SLUG` 改为 `release-signing`；
3. 推 tag。sign.ps1 / workflow / 其余 secrets 零改动。

> **待确认变量（已列入与 Foundation 的邮件）**：release 策略若强制 **origin verification**（SignPath GitHub App + `signpath/github-action-submit-signing-request@v3`），REST 直提不再可用，需重构 workflow（构建/签名/发布拆 job、签名后重算 minisign `.sig` 与 latest.json）。若仅要求审批，现脚本已兼容（轮询等待，超时经 `SIGNPATH_TIMEOUT_SECONDS` 调大）。`test-signing` 策略与测试证书**保留不删**，日后管道回归测试用。

## GitHub Secrets 配置

仓库 **Settings → Secrets and variables → Actions → New repository secret**：

| Name | 必填 | Value |
| --- | --- | --- |
| `SIGNPATH_API_TOKEN` | ✅ | 门户 CI 用户（`GitHub Actions CI`）详情页生成的 API token（**只显示一次**） |
| `SIGNPATH_SIGNING_POLICY_SLUG` | 可选 | 签名策略 slug，不配则用脚本内置默认 `test-signing`；正式证书 + release 策略就绪后改为 `release-signing` 完成切换（见上节） |

> 另有同形环境变量 `SIGNPATH_ORGANIZATION_ID` / `SIGNPATH_PROJECT_SLUG` / `SIGNPATH_TIMEOUT_SECONDS`（轮询超时秒数，默认 600）可覆盖脚本内置默认值，正常无需配置。

## 零成本验证签名管道（自签测试证书）

不买证书也可以完整跑通管道：SignPath 门户侧的测试证书（`We Health Tick Test` + `test-signing` 策略）即为此存在。`SIGNPATH_API_TOKEN` 填入 Secrets 后推测试 tag：

```bash
git tag v0.0.0-sign-test && git push origin v0.0.0-sign-test   # 事后删掉
```

CI 日志中应看到（`Build and release` 步骤，每个 Windows 产物一组）：

```
==> [sign.ps1] SignPath 模式签名: D:\a\...\target\release\WeHealthTick.exe
==> [sign.ps1] SignPath 请求已提交: https://app.signpath.io/Api/v1/.../SigningRequests/...
==> [sign.ps1] SignPath 签名完成，已回写: D:\a\...\target\release\WeHealthTick.exe
```

无凭证时则看到黄色跳过日志（两个 `跳过 Windows 代码签名` 行）——这同样是管道正常的证据。

## 验证签名结果

下载 Release 产物，在 Windows PowerShell 中：

```powershell
# 查看签名详情（SignerCertificate 非空即已签名）
Get-AuthenticodeSignature .\WeHealthTick-0.2.7-windows-x64-setup.exe | Format-List

# 链式验证（正式证书应通过；自签测试证书因不在系统信任链，Status 会是 NotTrusted
# 类结果，属预期——自签仅验证"签上了"，不验证"受信任"）
signtool verify /pa /v .\WeHealthTick-0.2.7-windows-x64-setup.exe
```

本地 Windows 开发机手动签名构建：

```powershell
$env:SIGNPATH_API_TOKEN = "<token>"
pnpm tauri build --config src-tauri/tauri.windows.conf.json
```

## 故障排查

| 现象 | 可能原因 |
| --- | --- |
| CI 日志出现「跳过 Windows 代码签名」 | `SIGNPATH_API_TOKEN` 未配置或名称拼写不一致（大小写敏感） |
| SignPath 401/403 | API token 失效（CI 用户详情页重新生成并更新 secret）；或 CI 用户不在策略的 Submitters 里 |
| SignPath 轮询到超时 | 策略勾了审批（请求卡在 WaitingForApproval 等人批）：门户批准，或调大 `SIGNPATH_TIMEOUT_SECONDS`；网络抖动则重跑 CI |
| SignPath 终态 Failed / Denied | 到门户该请求详情页看具体原因（如 malware 扫描误拦，可在策略上关闭该扫描） |
| 自签测试证书 `verify /pa` 不通过 | 预期行为（无第三方信任链），用 `Get-AuthenticodeSignature` 确认 `SignerCertificate` 主体为 `We Health Tick Test` 即管道正常 |
| 签名后仍被个别引擎检出 | 信誉需要时间积累；签名初期个别启发式引擎仍可能告警，可向该引擎厂商提交误报申诉 |
| macOS 矩阵构建失败 | 误把 `--config src-tauri/tauri.windows.conf.json` 传给了非 Windows 矩阵（Windows 专属配置） |
