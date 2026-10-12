import SkipNextIcon from '@mui/icons-material/SkipNext';
import { Box, IconButton, Typography } from '@mui/material';

interface SkipButtonProps {
  /** 按钮下方的小字文案（如「跳过 (1/3)」/「跳过本次提醒」），同时作为 aria-label。 */
  label: string;
  onClick: () => void;
  disabled?: boolean;
}

// 三个 phase 视图（Alerting/Breaking/Paused-quiet）共用的底部跳过按钮：
// 小号 IconButton（浅透明度 hover 加深）+ 下方 10 号 caption 文案，低视觉权重、防误触。
// 由 AlertingView/BreakingView 的内联实现提取而来，逐字保持原样式。
export function SkipButton({ label, onClick, disabled = false }: SkipButtonProps) {
  return (
    <Box sx={{ display: 'flex', flexDirection: 'column', alignItems: 'center' }}>
      <IconButton
        aria-label={label}
        onClick={onClick}
        size="small"
        color="secondary"
        disabled={disabled}
        sx={{ 'opacity': 0.5, 'transition': 'opacity 0.2s', '&:hover': { opacity: 0.8 } }}
      >
        <SkipNextIcon />
      </IconButton>
      <Typography variant="caption" color="text.disabled" sx={{ fontSize: 10 }}>
        {label}
      </Typography>
    </Box>
  );
}
