import { alpha, Box } from '@mui/material';
import { commands } from '@src/shared/bindings';
import { logOnError, unwrap } from '@src/shared/commands';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useCallback, useEffect, useRef } from 'react';
import { Navigate, Route, Routes } from 'react-router-dom';
import { AlertingView } from './components/AlertingView';
import { BreakingView } from './components/BreakingView';
import { PausedView } from './components/PausedView';
import { WaitingView } from './components/WaitingView';
import { WorkingView } from './components/WorkingView';
import { usePanelForm } from './hooks/usePanelForm';
import { usePhaseRoute } from './hooks/usePhaseRoute';
import { useTimerState } from './hooks/useTimerState';
import { PHASE_PATHS, phaseToPath } from './routes';

// 毛玻璃卡片的公共视觉语言（小窗根 Box 与全屏放大卡片共用）：
// backdropFilter / 边框 / flex 纵向布局逐字相同，抽常量防两处漂移；bgcolor 依赖 theme 留在各分支内。
const CARD_BASE = {
  display: 'flex',
  flexDirection: 'column',
  alignItems: 'center',
  backdropFilter: 'blur(28px) saturate(180%)',
  WebkitBackdropFilter: 'blur(28px) saturate(180%)',
  border: 1,
  borderColor: 'divider',
} as const;

export default function PanelApp() {
  const {
    isPaused,
    displayTime,
    progress,
    phase,
    currentWhisperReminder,
    currentHealthReminder,
    isLongBreak,
    nextBreakIsLong,
    breakSkipCount,
    breakSkipMax,
    skipBreakAllowed,
    workActionBarVisible,
    skipCountReminder,
    todaySkipCount,
    breakPaused,
    quietHours,
    dndHours,
    togglePause,
    reset,
    manualBreak,
    confirmBreak,
    confirmReturn,
    skipBreak,
    pauseSource,
    remainingSeconds,
  } = useTimerState();
  // 镜像层：phase → URL 单向同步（replace-only，见 hook 头注释）。useTimerState 留在本组件
  // 顶层且不进 Routes——路由元素以 props 内联接收，phase 切换不重挂 hook、不闪 INITIAL_STATE。
  usePhaseRoute(phase);
  // 形态镜像层：后端 PanelForm 受管状态 → 前端布局分支（小窗毛玻璃卡片 vs 全屏遮罩）。
  // 全屏仅存在于非 Working（Alerting 起接管、Working 退出），失焦隐藏天然豁免，无需特判。
  const form = usePanelForm();
  const isFullscreen = form === 'fullscreen';
  // 失焦自动隐藏判定（组合布尔，见 hideOnBlurRef 同步 effect）：
  //   - Working 小窗（常态流）
  //   - Paused(Dnd) 免打扰查看面板（「想看才看」流：托盘唤起、失焦/再点托盘收起）
  // 其余阶段（Alerting/Breaking/Waiting/Paused manual|quiet）常驻桌面，由后端 phase-changed 唤起。
  // 全屏形态显式豁免：免打扰面板理论上恒为 Tray 形态（后端 sync 归位），此处显式建模不依赖该隐含保证。
  const hideOnBlurRef = useRef(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const currentWin = getCurrentWindow();
    const unlisten = currentWin.onFocusChanged(({ payload: focused }) => {
      if (!focused && hideOnBlurRef.current) {
        currentWin.hide();
      }
    });
    return () => {
      unlisten
        .then(fn => fn())
        .catch(err => console.warn('[onFocusChanged] unlisten failed:', err));
    };
  }, []);

  // phase / pauseSource / form 由各自 hook 独占订阅，此处合并同步到组合布尔 ref
  // 供 onFocusChanged 闭包读取（避免 stale closure）。
  useEffect(() => {
    hideOnBlurRef.current
      = phase === 'working'
        || (phase === 'paused' && pauseSource === 'dnd' && form !== 'fullscreen');
  }, [phase, pauseSource, form]);

  // 休息时段「跳过本次提醒」：后端记录被跳过时段（本时段内不再自动弹窗，时段结束
  // 自动恢复不受影响），仅在命令成功后隐藏窗口——失败时保持弹窗可重试。
  const skipQuietReminder = useCallback(() => {
    void unwrap(commands.skipQuietReminder())
      .then(() => getCurrentWindow().hide())
      .catch(err => console.warn('[skipQuietReminder]', err));
  }, []);

  // 任意 phase 切换 / reminder 变化导致 root 高度变化时，重新 fitPanel 让窗口高度跟随。
  // ResizeObserver 在 observe 后会异步触发一次首回调，等价于原 mount 即 fit 的语义。
  // 全屏期间 fitPanel 被后端 fit_panel 守卫吸收（权威抑制点）；退出全屏布局回小窗后
  // RO 自然再触发一次 fitPanel(height)，与 exit_panel_fullscreen 的显式恢复构成幂等双保险。
  useEffect(() => {
    const root = rootRef.current;
    if (!root) {
      return;
    }

    let raf = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        const height = root.offsetHeight;
        void logOnError(commands.fitPanel(height), 'fitPanel');
      });
    });
    observer.observe(root);

    return () => {
      cancelAnimationFrame(raf);
      observer.disconnect();
    };
  }, []);

  return (
    <Box
      ref={rootRef}
      sx={theme => (isFullscreen
        ? {
            // 全屏遮罩：transparent 窗口本身无背景，必须由前端不透明填充铺满区域；
            // 0.96 透明度保留极轻微透出，视觉上仍是「接管屏幕」的强制语义。
            width: '100vw',
            height: '100vh',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            bgcolor: alpha(theme.palette.background.default, 0.96),
            userSelect: 'none',
            overflow: 'hidden',
          }
        : {
            ...CARD_BASE,
            width: 240,
            py: 2,
            px: 1.5,
            gap: 1.5,
            userSelect: 'none',
            bgcolor: alpha(theme.palette.background.default, 0.65),
            borderRadius: '12px',
          })}
    >
      <Box
        sx={theme => (isFullscreen
          ? {
              // 全屏时的居中放大毛玻璃卡片：复用小窗同款视觉语言（CARD_BASE），
              // 5 个 phase 视图组件零改动（内部均 width: 100% 自适应）。
              ...CARD_BASE,
              width: 420,
              py: 5,
              px: 4,
              gap: 2,
              bgcolor: alpha(theme.palette.background.default, 0.65),
              borderRadius: '24px',
            }
          : {
              // 小窗形态：卡片即根，此层透传（不产生额外盒模型，样式集中在根 Box）。
              display: 'contents',
            })}
      >
        {/* 声明式路由：path 是后端 Phase 状态机的单向镜像（usePhaseRoute 纠偏）。
            root Box（ResizeObserver 挂载体）保持在 Routes 外部且永不卸载，高度自适应链路不受路由影响。
            中间卡片层同样常驻不卸载，form 切换只改样式不重挂路由树。
            '/' 与未知路径 replace 归一到当前 phase（Phase 为闭集，无额外兜底分支需求）。 */}
        <Routes>
          <Route path="/" element={<Navigate to={phaseToPath(phase)} replace />} />
          <Route
            path={PHASE_PATHS.working}
            element={(
              <WorkingView
                displayTime={displayTime}
                progress={progress}
                isPaused={isPaused}
                nextBreakIsLong={nextBreakIsLong}
                actionBarVisible={workActionBarVisible}
                onToggle={togglePause}
                onReset={reset}
                onManualBreak={manualBreak}
              />
            )}
          />
          <Route
            path={PHASE_PATHS.alerting}
            element={(
              <AlertingView
                whisperReminder={currentWhisperReminder}
                isLongBreak={isLongBreak}
                breakSkipCount={breakSkipCount}
                breakSkipMax={breakSkipMax}
                todaySkipCount={todaySkipCount}
                skipCountReminder={skipCountReminder}
                skipAllowed={skipBreakAllowed}
                onStartBreak={confirmBreak}
                onSkip={skipBreak}
              />
            )}
          />
          <Route
            path={PHASE_PATHS.breaking}
            element={(
              <BreakingView
                displayTime={displayTime}
                progress={progress}
                whisperReminder={currentWhisperReminder}
                healthReminder={currentHealthReminder}
                isLongBreak={isLongBreak}
                breakSkipCount={breakSkipCount}
                breakSkipMax={breakSkipMax}
                breakPaused={breakPaused}
                skipAllowed={skipBreakAllowed}
                onSkip={skipBreak}
              />
            )}
          />
          <Route
            path={PHASE_PATHS.waiting}
            element={(
              <WaitingView
                isLongBreak={isLongBreak}
                onReturn={confirmReturn}
              />
            )}
          />
          <Route
            path={PHASE_PATHS.paused}
            element={(
              <PausedView
                remainingSeconds={remainingSeconds}
                pauseSource={pauseSource}
                quietHours={quietHours}
                dndHours={dndHours}
                onResume={togglePause}
                onSkipQuiet={skipQuietReminder}
              />
            )}
          />
          <Route path="*" element={<Navigate to={phaseToPath(phase)} replace />} />
        </Routes>
      </Box>
    </Box>
  );
}
