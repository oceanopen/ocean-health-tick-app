import type { DownloadEvent, Update } from '@tauri-apps/plugin-updater';
import AutorenewIcon from '@mui/icons-material/Autorenew';
import DownloadIcon from '@mui/icons-material/Download';
import FavoriteIcon from '@mui/icons-material/Favorite';
import { Box, Button, Chip, CircularProgress, Typography } from '@mui/material';
import appIcon from '@src/assets/app-icon.png';
import { relaunch } from '@tauri-apps/plugin-process';
import { check } from '@tauri-apps/plugin-updater';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

declare const __APP_VERSION__: string;

// updater 检查与下载安装的状态机。
// idle/checking/up-to-date/error 为「检查」分支；
// available/downloading/downloaded 为「下载安装」分支。
// downloadAndInstall 完成后 plugin-updater 不会自动重启，downloaded 状态由用户点按钮触发 relaunch。
type CheckState
  = | { kind: 'idle' }
    | { kind: 'checking' }
    | { kind: 'up-to-date' }
    | { kind: 'available'; update: Update }
    // percent === null 表示下载源未返回 Content-Length（拿不到真实百分比），回退展示已下载字节数。
    | { kind: 'downloading'; percent: number | null; downloadedBytes: number }
    | { kind: 'downloaded' }
    | { kind: 'error'; message: string };

// 把已下载字节数格式化为带单位的可读字符串：< 1MB 显示 KB，≥ 1MB 显示 MB（保留 1 位小数）。
function formatDownloadedSize(bytes: number): string {
  if (bytes < 1024 * 1024) {
    return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  }
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function AboutPage() {
  const { t } = useTranslation();
  const [state, setState] = useState<CheckState>({ kind: 'idle' });
  // 跟踪当前持有的 Update 句柄，供 handleCheck 重试、下载 finally、unmount cleanup 统一 close。
  const updateRef = useRef<Update | null>(null);

  // unmount 时释放可能残留的 Update，覆盖下载中途关闭页面等场景。
  useEffect(() => {
    return () => {
      updateRef.current?.close().catch(() => {});
      updateRef.current = null;
    };
  }, []);

  const handleCheck = async () => {
    // Update extends Resource，需显式 close 释放 Rust 端句柄。
    // 无论之前是 available / error(持有上一次 update) / downloaded，重新检查前都先释放，避免累积泄漏。
    if (updateRef.current) {
      await updateRef.current.close().catch(() => {});
      updateRef.current = null;
    }
    setState({ kind: 'checking' });
    try {
      // 弱网下 plugin-updater 默认无应用层超时，会等到 OS/TCP 层才失败（最坏几分钟）。
      // 单源 5s 硬超时：plugin v2 的 timeout 是 per-endpoint（每个 endpoint 独立享有一个 reqwest
      // ClientBuilder.timeout 窗口），endpoints 数组 [GitHub, Gitee] 串行回退，最坏合计 10s。
      // 超时后落入下方 catch 的 error 状态，用户可点「重试」。
      const update = await check({ timeout: 5_000 });
      if (update) {
        updateRef.current = update;
        setState({ kind: 'available', update });
      } else {
        setState({ kind: 'up-to-date' });
      }
    } catch (e) {
      setState({ kind: 'error', message: e instanceof Error ? e.message : String(e) });
    }
  };

  const handleDownloadAndInstall = async (update: Update) => {
    setState({ kind: 'downloading', percent: null, downloadedBytes: 0 });
    let total = 0;
    let downloaded = 0;
    try {
      await update.downloadAndInstall((event: DownloadEvent) => {
        switch (event.event) {
          case 'Started':
            total = event.data.contentLength ?? 0;
            break;
          case 'Progress':
            downloaded += event.data.chunkLength;
            if (total > 0) {
              // 有真实 Content-Length：按百分比上报。
              const percent = Math.min(100, Math.round((downloaded / total) * 100));
              setState({ kind: 'downloading', percent, downloadedBytes: downloaded });
            } else {
              // 缺失 Content-Length（如 Gitee 302 重定向链路）：回退展示已下载字节数，避免卡在 0%。
              setState({ kind: 'downloading', percent: null, downloadedBytes: downloaded });
            }
            break;
          case 'Finished':
            break;
        }
      });
      // downloadAndInstall 完成后 plugin-updater 不会自动重启，
      // 落到 downloaded 状态等用户点「重启」按钮触发 relaunch。
      setState({ kind: 'downloaded' });
    } catch (e) {
      setState({ kind: 'error', message: e instanceof Error ? e.message : String(e) });
    } finally {
      // 无论下载成功还是失败，Rust 端句柄都不再需要，统一释放。
      await updateRef.current?.close().catch(() => {});
      updateRef.current = null;
    }
  };

  const buttonProps = (() => {
    switch (state.kind) {
      case 'checking':
        return {
          label: t('about:checking'),
          icon: <CircularProgress size={16} />,
          disabled: true,
          onClick: undefined,
        };
      case 'available':
        return {
          label: t('about:downloadAndInstall', { version: state.update.version }),
          icon: <DownloadIcon />,
          disabled: false,
          onClick: () => handleDownloadAndInstall(state.update),
        };
      case 'downloading':
        return {
          label: state.percent === null
            ? t('about:downloadingSize', { size: formatDownloadedSize(state.downloadedBytes) })
            : t('about:downloadingPercent', { percent: state.percent }),
          icon: <CircularProgress size={16} />,
          disabled: true,
          onClick: undefined,
        };
      case 'downloaded':
        return {
          label: t('about:relaunch'),
          icon: <AutorenewIcon />,
          disabled: false,
          onClick: () => relaunch(),
        };
      case 'error':
        return {
          label: t('about:retry'),
          icon: <AutorenewIcon />,
          disabled: false,
          onClick: handleCheck,
        };
      case 'idle':
      case 'up-to-date':
      default:
        return {
          label: t('about:checkUpdate'),
          icon: <AutorenewIcon />,
          disabled: false,
          onClick: handleCheck,
        };
    }
  })();

  const statusText = (() => {
    switch (state.kind) {
      case 'up-to-date':
        return t('about:upToDate', { version: __APP_VERSION__ });
      case 'available':
        return t('about:updateAvailable', { version: state.update.version });
      case 'downloaded':
        return t('about:updateReady');
      case 'error':
        return t('about:checkFailed', { error: state.message });
      default:
        return null;
    }
  })();

  return (
    <Box
      sx={{
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        height: '100%',
        gap: 1.5,
        px: 3,
        py: 4,
      }}
    >
      <Box
        component="img"
        src={appIcon}
        alt={t('common:brand')}
        sx={{
          width: 80,
          height: 80,
          borderRadius: 3,
          mb: 1,
        }}
      />

      <Typography variant="h6" sx={{ fontWeight: 'bold' }}>
        {t('common:brand')}
      </Typography>

      <Typography variant="body2" color="text.secondary">
        {t('about:subtitle')}
      </Typography>

      <Chip label={`v${__APP_VERSION__}`} size="small" />

      <Typography variant="body2" color="text.secondary">
        {t('about:slogan')}
      </Typography>

      <Button
        variant="outlined"
        size="small"
        startIcon={buttonProps.icon}
        disabled={buttonProps.disabled}
        sx={{ mt: 1, textTransform: 'none', minWidth: 160 }}
        onClick={buttonProps.onClick}
      >
        {buttonProps.label}
      </Button>

      <Typography variant="body2" color="text.secondary" sx={{ minHeight: 20 }}>
        {statusText ?? ''}
      </Typography>

      <Typography
        component="a"
        role="button"
        variant="body2"
        color="primary"
        sx={{
          'textDecoration': 'none',
          'cursor': 'pointer',
          '&:hover': { textDecoration: 'underline' },
          'mt': 1,
        }}
        onClick={() => {
          import('@tauri-apps/plugin-shell').then(({ open }) => {
            open('https://github.com/oceanopen/ocean-health-tick-app');
          });
        }}
      >
        GitHub
      </Typography>

      <Box sx={{ display: 'flex', alignItems: 'center', gap: 0.5, mt: 'auto', pt: 2 }}>
        <Typography variant="caption" color="text.secondary">
          {t('about:madeWith')}
        </Typography>
        <FavoriteIcon sx={{ fontSize: 12, color: 'error.main' }} />
        <Typography variant="caption" color="text.secondary">
          {t('about:forHealth')}
        </Typography>
      </Box>
    </Box>
  );
}

export default AboutPage;
