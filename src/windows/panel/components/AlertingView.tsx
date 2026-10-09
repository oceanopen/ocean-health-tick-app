import InfoIcon from '@mui/icons-material/Info';
import NotificationsActiveIcon from '@mui/icons-material/NotificationsActive';
import SkipNextIcon from '@mui/icons-material/SkipNext';
import { Box, Button, IconButton, Typography, useTheme } from '@mui/material';
import { useTranslation } from 'react-i18next';
import { PHASE_RING_COLORS } from '../phaseColors';

interface AlertingViewProps {
  whisperReminder: string;
  isLongBreak: boolean;
  breakSkipCount: number;
  breakSkipMax: number;
  todaySkipCount: number;
  skipCountReminder: number;
  skipAllowed: boolean;
  onStartBreak: () => void;
  onSkip: () => void;
}

export function AlertingView({ whisperReminder, isLongBreak, breakSkipCount, breakSkipMax, todaySkipCount, skipCountReminder, skipAllowed, onStartBreak, onSkip }: AlertingViewProps) {
  const { t } = useTranslation();
  const theme = useTheme();
  const alertingColor
    = theme.palette.mode === 'light' ? PHASE_RING_COLORS.alerting.light : PHASE_RING_COLORS.alerting.dark;
  const skipLabel = `${t('panel:action.skipBreak')} (${breakSkipCount}/${breakSkipMax})`;
  // 即将进入的休息类型（on_work_done 提前写入）区分标题，确认前用户即知休息类型。
  const alertTitle = isLongBreak ? t('panel:alertLongTitle') : t('panel:alertTitle');
  // 今日累计「真正跳过休息」≥ 阈值且阈值 > 0（未关闭）时显示警示横幅；阈值 0 = 关闭提醒。
  const showSkipWarning = skipCountReminder > 0 && todaySkipCount >= skipCountReminder;

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
      <NotificationsActiveIcon sx={{ fontSize: 120, color: alertingColor }} />
      <Typography variant="subtitle1">
        {alertTitle}
      </Typography>
      {/* pre-line：文案中的显式换行符渲染为真实换行（white-space: normal 会把 \n 折叠成空格） */}
      <Typography variant="caption" align="center" color="text.secondary" sx={{ px: 1, whiteSpace: 'pre-line' }}>
        {whisperReminder}
      </Typography>
      {showSkipWarning && (
        <Box
          sx={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: 0.75,
            width: '100%',
            px: 1.25,
            py: 0.75,
            borderRadius: 1,
            bgcolor: 'warning.main',
            color: 'warning.contrastText',
          }}
        >
          <InfoIcon sx={{ fontSize: 16 }} />
          <Typography variant="caption" sx={{ fontSize: 11, lineHeight: 1 }}>
            {t('panel:skipWarning')}
          </Typography>
        </Box>
      )}
      <Button variant="contained" fullWidth onClick={onStartBreak} sx={{ mt: 1, textTransform: 'none' }}>
        {t('panel:action.startBreak')}
      </Button>
      <Box sx={{ display: 'flex', flexDirection: 'column', alignItems: 'center' }}>
        <IconButton
          aria-label={skipLabel}
          onClick={onSkip}
          size="small"
          color="secondary"
          disabled={!skipAllowed}
          sx={{ 'opacity': 0.5, 'transition': 'opacity 0.2s', '&:hover': { opacity: 0.8 } }}
        >
          <SkipNextIcon />
        </IconButton>
        <Typography variant="caption" color="text.disabled" sx={{ fontSize: 10 }}>
          {skipLabel}
        </Typography>
      </Box>
    </Box>
  );
}
