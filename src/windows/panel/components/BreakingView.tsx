import DirectionsRunIcon from '@mui/icons-material/DirectionsRun';
import InfoIcon from '@mui/icons-material/Info';
import SkipNextIcon from '@mui/icons-material/SkipNext';
import { Box, Divider, IconButton, Typography } from '@mui/material';
import { useTranslation } from 'react-i18next';
import { CountdownRing } from './CountdownRing';

interface BreakingViewProps {
  displayTime: string;
  progress: number;
  whisperReminder: string;
  healthReminder: string;
  isLongBreak: boolean;
  breakSkipCount: number;
  breakSkipMax: number;
  breakPaused: boolean;
  skipAllowed: boolean;
  onSkip: () => void;
}

export function BreakingView({
  displayTime,
  progress,
  whisperReminder,
  healthReminder,
  isLongBreak,
  breakSkipCount,
  breakSkipMax,
  breakPaused,
  skipAllowed,
  onSkip,
}: BreakingViewProps) {
  const { t } = useTranslation();
  const phaseLabel = isLongBreak ? t('panel:longBreakLabel') : t('panel:phaseBreaking');
  const skipLabel = `${t('panel:action.skipBreak')} (${breakSkipCount}/${breakSkipMax})`;

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
      <CountdownRing phase="breaking" displayTime={displayTime} progress={progress} />
      {breakPaused && (
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
            {t('panel:breakActivityPaused')}
          </Typography>
        </Box>
      )}
      <Typography variant="subtitle1">
        {phaseLabel}
      </Typography>
      {healthReminder && (
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
            bgcolor: 'success.main',
            color: 'success.contrastText',
          }}
        >
          <DirectionsRunIcon sx={{ fontSize: 16 }} />
          {/* pre-line：自定义文案可含显式换行符，渲染为真实换行而非折叠成空格 */}
          <Typography variant="caption" sx={{ fontSize: 11, lineHeight: 1.3, whiteSpace: 'pre-line' }}>
            {healthReminder}
          </Typography>
        </Box>
      )}
      {whisperReminder && (
        <Typography variant="caption" align="center" color="text.secondary" sx={{ px: 1, whiteSpace: 'pre-line' }}>
          {whisperReminder}
        </Typography>
      )}
      <Divider sx={{ width: '100%' }} />
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
