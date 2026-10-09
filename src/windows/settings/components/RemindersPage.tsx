import AddOutlinedIcon from '@mui/icons-material/AddOutlined';
import CloseOutlinedIcon from '@mui/icons-material/CloseOutlined';
import {
  Box,
  Button,
  Divider,
  IconButton,
  InputAdornment,
  Tab,
  Tabs,
  TextField,
  Typography,
} from '@mui/material';
import {
  decodeAppRemindersConfig,
  encodeAppRemindersConfig,
  getAppConfig,
  REMINDERS_KEY,
  setAppConfig,
} from '@src/shared/appConfig';
import {
  DEFAULT_HEALTH_TEXTS,
  DEFAULT_WHISPER_TEXTS,
} from '@src/shared/bindings';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

interface ReminderItem {
  id: number;
  text: string;
}

// 两类文案的草稿/已保存结构（带前端运行时 id，id 不参与持久化）。
interface ReminderList {
  health: ReminderItem[];
  whisper: ReminderItem[];
}

type ReminderTab = 'health' | 'whisper';

const MAX_REMINDER_LENGTH = 500;

function isInvalidReminder(text: string): boolean {
  return text.trim() === '' || text.length > MAX_REMINDER_LENGTH;
}

// 默认文案常量从 bindings 导入（SSOT 为后端 shared/reminder_texts.rs，
// 经 build_specta_builder().constant() 自动生成，类型为 readonly string[]）。

function RemindersPage() {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<ReminderTab>('health');
  const [hasValidationAttempted, setHasValidationAttempted] = useState(false);
  const reminderIdRef = useRef(0);

  // 自增 id 生成器：保证两类列表合并后所有 id 全局唯一，避免增删时输入框错位。
  // 仅依赖 ref（稳定），故空依赖；stable 引用可安全用于 useEffect 依赖数组。
  const allocateReminderId = useCallback(() => {
    reminderIdRef.current += 1;
    return reminderIdRef.current;
  }, []);

  // 把纯文案数组包装成带 id 的条目（持久化只存 text，id 仅作 React key）。
  // 入参放宽为 readonly string[]：默认文案来自 bindings 的 as const 常量。
  const buildItems = useCallback(
    (texts: readonly string[]): ReminderItem[] =>
      texts.map(text => ({ id: allocateReminderId(), text })),
    [allocateReminderId],
  );

  const [saved, setSaved] = useState<ReminderList>(() => ({
    health: buildItems(DEFAULT_HEALTH_TEXTS),
    whisper: buildItems(DEFAULT_WHISPER_TEXTS),
  }));
  const [draft, setDraft] = useState<ReminderList>(saved);

  useEffect(() => {
    getAppConfig(REMINDERS_KEY).then((raw) => {
      const decoded = decodeAppRemindersConfig(raw);
      const health = decoded.health.length > 0
        ? buildItems(decoded.health)
        : buildItems(DEFAULT_HEALTH_TEXTS);
      const whisper = decoded.whisper.length > 0
        ? buildItems(decoded.whisper)
        : buildItems(DEFAULT_WHISPER_TEXTS);
      const next: ReminderList = { health, whisper };
      setSaved(next);
      setDraft(next);
      setHasValidationAttempted(false);
    });
  }, [buildItems]);

  // —— CRUD 仅作用于当前激活 Tab 的列表 ——
  const updateReminder = (index: number, text: string) => {
    setDraft(prev => ({
      ...prev,
      [activeTab]: prev[activeTab].map((r, i) => (i === index ? { ...r, text } : r)),
    }));
  };

  const addReminder = () => {
    setDraft(prev => ({
      ...prev,
      [activeTab]: [...prev[activeTab], { id: allocateReminderId(), text: '' }],
    }));
  };

  const removeReminder = (index: number) => {
    setDraft(prev => ({
      ...prev,
      [activeTab]: prev[activeTab].filter((_, i) => i !== index),
    }));
  };

  // dirty 按当前 Tab 判断：底部按钮「共用但分别处理」。
  const dirty
    = JSON.stringify(saved[activeTab]) !== JSON.stringify(draft[activeTab]);

  const handleReset = () => {
    const texts: readonly string[] = activeTab === 'health'
      ? DEFAULT_HEALTH_TEXTS
      : DEFAULT_WHISPER_TEXTS;
    setDraft(prev => ({ ...prev, [activeTab]: buildItems(texts) }));
    setHasValidationAttempted(false);
  };
  const handleCancel = () => {
    setDraft(prev => ({ ...prev, [activeTab]: saved[activeTab] }));
    setHasValidationAttempted(false);
  };

  const handleSave = async () => {
    if (draft[activeTab].some(r => isInvalidReminder(r.text))) {
      setHasValidationAttempted(true);
      return;
    }
    setHasValidationAttempted(false);
    // 单 key 结构化对象：当前 Tab 用 draft，另一类保留其 saved 值（互不污染）。
    const merged = {
      health: (activeTab === 'health' ? draft.health : saved.health).map(({ text }) => text),
      whisper: (activeTab === 'whisper' ? draft.whisper : saved.whisper).map(({ text }) => text),
    };
    await setAppConfig(REMINDERS_KEY, encodeAppRemindersConfig(merged));
    setSaved(prev => ({ ...prev, [activeTab]: draft[activeTab] }));
  };

  const items = draft[activeTab];

  return (
    <Box sx={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <Box sx={{ p: 3, flex: 1, overflow: 'auto' }}>
        <Tabs
          value={activeTab}
          onChange={(_, value: ReminderTab) => {
            setActiveTab(value);
            setHasValidationAttempted(false);
          }}
          sx={{ borderBottom: 1, borderColor: 'divider', mb: 2 }}
        >
          <Tab value="health" label={t('reminders:tab.health')} />
          <Tab value="whisper" label={t('reminders:tab.whisper')} />
        </Tabs>

        <Box
          sx={{
            borderRadius: 2,
            border: 1,
            borderColor: 'divider',
            overflow: 'hidden',
          }}
        >
          <Box sx={{ px: 2, py: 1.5 }}>
            <Typography variant="caption" sx={{ opacity: 0.7 }}>
              {t('reminders:page.hint')}
            </Typography>
            <Box sx={{ display: 'flex', flexDirection: 'column', gap: 1, mt: 2 }}>
              {items.map((r, i) => (
                <Box key={r.id} sx={{ display: 'flex', gap: 1, alignItems: 'flex-start' }}>
                  <TextField
                    fullWidth
                    multiline
                    minRows={1}
                    maxRows={8}
                    size="small"
                    value={r.text}
                    placeholder={t('reminders:row.placeholder')}
                    onChange={e => updateReminder(i, e.target.value)}
                    error={hasValidationAttempted && isInvalidReminder(r.text)}
                    sx={{ flex: 1 }}
                    slotProps={{
                      input: {
                        endAdornment: (
                          <InputAdornment
                            position="end"
                            sx={{ alignSelf: 'flex-end' }}
                          >
                            <Typography
                              variant="caption"
                              component="span"
                              sx={{
                                fontSize: 10,
                                fontVariantNumeric: 'tabular-nums',
                                minWidth: '3.5em',
                                textAlign: 'right',
                                color: r.text.length > MAX_REMINDER_LENGTH
                                  ? 'error.main'
                                  : 'text.disabled',
                              }}
                            >
                              {r.text.length}
                              /
                              {MAX_REMINDER_LENGTH}
                            </Typography>
                          </InputAdornment>
                        ),
                      },
                    }}
                  />
                  {items.length > 1 && (
                    <IconButton size="small" onClick={() => removeReminder(i)}>
                      <CloseOutlinedIcon fontSize="small" />
                    </IconButton>
                  )}
                </Box>
              ))}
            </Box>
            <Divider sx={{ my: 2 }} />
            <Button
              size="small"
              startIcon={<AddOutlinedIcon />}
              onClick={addReminder}
              color="inherit"
            >
              {t('reminders:button.add')}
            </Button>
          </Box>
        </Box>
      </Box>

      <Box
        sx={{
          p: 2,
          borderTop: 1,
          borderColor: 'divider',
          display: 'flex',
          justifyContent: 'flex-end',
          gap: 1,
        }}
      >
        <Button onClick={handleReset} color="inherit">
          {t('reminders:button.reset')}
        </Button>
        <Button onClick={handleCancel} disabled={!dirty} color="inherit">
          {t('reminders:button.cancel')}
        </Button>
        <Button onClick={handleSave} disabled={!dirty} variant="contained">
          {t('reminders:button.save')}
        </Button>
      </Box>
    </Box>
  );
}

export default RemindersPage;
