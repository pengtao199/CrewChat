/**
 * One row per API key: a remark, a hidden secret, and an eye that reveals only
 * that row. The list endpoint never carries the secret; the eye asks for it.
 */

import type { IProviderKey, IProviderKeyInput } from '@/common/types/provider/providerApi';
import { Button, Input, Message } from '@arco-design/web-react';
import { Delete, Plus, PreviewClose, PreviewOpen } from '@icon-park/react';
import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';

const MASK = '••••••••';

export type KeyDraft = {
  id: string;
  remark: string;
  secret: string;
  is_default: boolean;
  has_secret: boolean;
  dirty: boolean;
  visible: boolean;
};

export const draftsFromKeys = (keys: IProviderKey[] | undefined): KeyDraft[] => {
  const rows = (keys ?? []).map((key) => ({
    id: key.id,
    remark: key.remark,
    secret: '',
    is_default: key.is_default,
    has_secret: key.has_secret,
    dirty: false,
    visible: false,
  }));
  if (!rows.length) {
    return [emptyDraft(true)];
  }
  if (!rows.some((row) => row.is_default)) {
    rows[0] = { ...rows[0], is_default: true };
  }
  return rows;
};

const emptyDraft = (isDefault: boolean): KeyDraft => ({
  id: `new-${crypto.randomUUID()}`,
  remark: '',
  secret: '',
  is_default: isDefault,
  has_secret: false,
  dirty: false,
  visible: true,
});

/** Omit `secret` when the user did not type a new one, so the server keeps it. */
export const keyInputsFromDrafts = (drafts: KeyDraft[]): IProviderKeyInput[] =>
  drafts
    .filter((draft) => (draft.dirty ? draft.secret.trim().length > 0 : draft.has_secret))
    .map((draft) => ({
      id: draft.id,
      remark: draft.remark.trim(),
      is_default: draft.is_default,
      ...(draft.dirty ? { secret: draft.secret.trim() } : {}),
    }));

type ProviderKeyListProps = {
  drafts: KeyDraft[];
  onChange: (drafts: KeyDraft[]) => void;
  onReveal: (keyId: string) => Promise<string>;
};

const ProviderKeyList: React.FC<ProviderKeyListProps> = ({ drafts, onChange, onReveal }) => {
  const { t } = useTranslation();
  const [message, messageContext] = Message.useMessage();
  const [revealing, setRevealing] = useState('');

  const update = (id: string, patch: Partial<KeyDraft>) => {
    onChange(drafts.map((draft) => (draft.id === id ? { ...draft, ...patch } : draft)));
  };

  const shownValue = (draft: KeyDraft) => {
    if (draft.visible || draft.dirty) return draft.secret;
    if (draft.has_secret) return MASK;
    return '';
  };

  const reveal = async (draft: KeyDraft) => {
    if (draft.visible) {
      update(draft.id, { visible: false });
      return;
    }
    if (!draft.secret && draft.has_secret && !draft.id.startsWith('new-')) {
      setRevealing(draft.id);
      try {
        const secret = await onReveal(draft.id);
        onChange(
          drafts.map((row) => (row.id === draft.id ? { ...row, secret, visible: true, dirty: false } : row))
        );
      } catch (error) {
        message.error(error instanceof Error ? error.message : t('settings.models.revealFailed'));
      } finally {
        setRevealing('');
      }
      return;
    }
    update(draft.id, { visible: true });
  };

  return (
    <div className='flex flex-col gap-10px'>
      {messageContext}
      {drafts.map((draft, index) => (
        <div
          key={draft.id}
          className='flex flex-col gap-8px rounded-12px border border-solid border-border-2 bg-fill-1 px-12px py-12px'
        >
          <div className='flex items-center justify-between gap-8px'>
            <span className='text-12px font-medium text-t-secondary'>
              {t('settings.models.keyAccount', { index: index + 1 })}
            </span>
            <div className='flex items-center gap-6px'>
              <Button
                size='mini'
                type={draft.is_default ? 'primary' : 'outline'}
                onClick={() =>
                  onChange(drafts.map((row) => ({ ...row, is_default: row.id === draft.id })))
                }
              >
                {t('settings.models.keyDefault')}
              </Button>
              <Button
                size='mini'
                type='text'
                disabled={drafts.length === 1}
                icon={<Delete size={14} />}
                onClick={() => {
                  const rest = drafts.filter((row) => row.id !== draft.id);
                  if (draft.is_default && rest.length) rest[0] = { ...rest[0], is_default: true };
                  onChange(rest);
                }}
              />
            </div>
          </div>
          <Input
            value={draft.remark}
            placeholder={t('settings.models.keyRemarkPlaceholder')}
            onChange={(remark) => update(draft.id, { remark })}
          />
          <Input.Password
            value={shownValue(draft)}
            placeholder={t('settings.models.keySecretPlaceholder')}
            visibility={draft.visible}
            visibilityToggle={false}
            onChange={(value) => {
              let next = value;
              if (!draft.dirty && !draft.visible && draft.has_secret && next.startsWith(MASK)) {
                next = next.slice(MASK.length);
              }
              update(draft.id, { secret: next, dirty: true, has_secret: true, visible: true });
            }}
            suffix={
              <Button
                type='text'
                size='mini'
                loading={revealing === draft.id}
                icon={draft.visible ? <PreviewClose size={14} /> : <PreviewOpen size={14} />}
                onClick={() => void reveal(draft)}
              />
            }
          />
        </div>
      ))}
      <Button
        type='outline'
        long
        icon={<Plus size={14} />}
        onClick={() => onChange([...drafts, emptyDraft(drafts.length === 0)])}
      >
        {t('settings.models.keyAdd')}
      </Button>
    </div>
  );
};

export default ProviderKeyList;
