import type { DndHours, QuietHourPeriod, QuietHours } from '@src/shared/appConfig';
import type { PauseSource } from '@src/shared/bindings';
import PauseCircleFilledIcon from '@mui/icons-material/PauseCircleFilled';
import { Box, Button, Typography, useTheme } from '@mui/material';
import { useTranslation } from 'react-i18next';
import { PHASE_RING_COLORS } from '../phaseColors';
import { SkipButton } from './SkipButton';

interface PausedViewProps {
  remainingSeconds: number;
  pauseSource: PauseSource;
  quietHours: QuietHours;
  dndHours: DndHours;
  onResume: () => void;
  onSkipQuiet: () => void;
}

function formatHMS(seconds: number): string {
  const safe = Math.max(0, Math.floor(seconds));
  const h = Math.floor(safe / 3600);
  const m = Math.floor((safe % 3600) / 60);
  const s = safe % 60;
  return [h, m, s].map(n => String(n).padStart(2, '0')).join(':');
}

function nowHHmm(): string {
  const d = new Date();
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

// 当前命中的时段条目（与后端 is_in_periods 同逻辑，含跨午夜 start > end）。
// quiet_hours / dnd_hours 两列表共用；无命中返回 null。
function findActivePeriod(
  periods: readonly QuietHourPeriod[],
  now: string,
): QuietHourPeriod | null {
  for (const p of periods) {
    if (p.start <= p.end) {
      if (p.start <= now && now < p.end) {
        return p;
      }
    } else if (now >= p.start || now < p.end) {
      return p;
    }
  }
  return null;
}

// 时段暂停 icon 配色决策（颜色收敛，不新增色）：全部复用 PHASE_RING_COLORS——
//   - 休息时段 / 手动暂停 = paused 灰（暂停类统一灰，二者靠标题「休息时段中/已暂停」区分）
//   - 免打扰 = working 绿（静默让位于专注，视觉上等同工作中、不引起额外注意）

export function PausedView({
  remainingSeconds,
  pauseSource,
  quietHours,
  dndHours,
  onResume,
  onSkipQuiet,
}: PausedViewProps) {
  const { t } = useTranslation();
  const theme = useTheme();

  // 按暂停来源三态显式分派：manual（已暂停，可恢复）/ quiet（休息时段中，打断式）/
  // dnd（免打扰时段中，非打断式）。后两者继续按钮禁用 + 显示命中时段范围 + 自动恢复提示。
  const isQuiet = pauseSource === 'quiet';
  const isDnd = pauseSource === 'dnd';
  // icon 三态显式分派：dnd=working 绿；quiet/manual=paused 灰（见上方配色决策注释）。
  const mode = theme.palette.mode === 'light' ? 'light' : 'dark';
  const iconColor = isDnd
    ? PHASE_RING_COLORS.working[mode]
    : PHASE_RING_COLORS.paused[mode];
  const title = isQuiet
    ? t('panel:quietHoursActive')
    : isDnd
      ? t('panel:dndHoursActive')
      : t('panel:phasePaused');

  const periods = isQuiet ? quietHours : isDnd ? dndHours : null;
  const activePeriod = periods ? findActivePeriod(periods, nowHHmm()) : null;
  const showRange = activePeriod !== null;
  const displayText = activePeriod
    ? `${activePeriod.start}:00 - ${activePeriod.end}:00`
    : formatHMS(remainingSeconds);

  return (
    <Box
      sx={{
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        gap: 1.5,
        width: '100%',
        py: 1,
      }}
    >
      <PauseCircleFilledIcon sx={{ fontSize: 120, color: iconColor }} />
      <Typography variant="subtitle1" component="div">
        {title}
      </Typography>
      <Typography
        variant="caption"
        component="div"
        align="center"
        color="text.secondary"
        sx={{ px: 1, fontVariantNumeric: 'tabular-nums' }}
      >
        {displayText}
      </Typography>
      <Box sx={{ display: 'flex', flexDirection: 'column', gap: 0.5 }}>
        <Typography variant="caption" align="center" color="text.secondary" sx={{ px: 1, opacity: 0.7 }}>
          {showRange
            ? (isDnd ? t('panel:dndHoursRangeLabel') : t('panel:quietHoursRangeLabel'))
            : t('panel:resumableRemainingLabel')}
        </Typography>
        {(isQuiet || isDnd) && (
          <Typography variant="caption" align="center" color="text.secondary" sx={{ px: 1, opacity: 0.7 }}>
            {t(isDnd ? 'panel:dndAutoResumeHint' : 'panel:pausedAutoResumeHint')}
          </Typography>
        )}
      </Box>
      <Button
        variant="contained"
        fullWidth
        onClick={onResume}
        disabled={isQuiet || isDnd}
        sx={{ mt: 1, textTransform: 'none' }}
      >
        {t('panel:action.resume')}
      </Button>
      {/* 休息时段（Quiet）独有：「跳过」——本时段内不再弹出提醒窗口，
          计时暂停与时段结束自动恢复不受影响；manual/dnd 状态不显示。
          复用与其他休息窗口一致的底部 SkipButton 形态与「跳过」文案；
          单击生效（不占用休息轮次，无需防误触连点）。 */}
      {isQuiet && <SkipButton label={t('panel:action.skipBreak')} onClick={onSkipQuiet} />}
    </Box>
  );
}
