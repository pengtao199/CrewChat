/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

import { ipcBridge } from '@/common';
import type { UsageSummary } from '@/common/types/usage/usageApi';
import { Button, Radio, Table } from '@arco-design/web-react';
import { Refresh } from '@icon-park/react';
import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import useSWR from 'swr';
import AionScrollArea from '@/renderer/components/base/AionScrollArea';
import EmptyState from '@/renderer/components/base/EmptyState';
import SettingsCardList from '@/renderer/components/base/SettingsCardList';
import DataLocationNote from '@/renderer/components/settings/DataLocationNote';
import SettingsPageHeader from '@/renderer/pages/settings/components/SettingsPageHeader';
import { useSettingsViewMode } from '../settingsViewContext';

/**
 * Settings → 用量.
 *
 * 只展示**由 CrewChat 自己发起的 CLI 身体**所消耗的 token 与成本：数据来自我们
 * 自己的记账（`.data/crewchat.db`），不读网关日志，因此天然排除用户在终端里裸跑
 * CLI 的用量。`llm` 是内置 API 客户端、不是 CLI 身体，不计入。
 */

const DAY_OPTIONS = [7, 30, 90] as const;

const fmtInt = (n: number | undefined): string => new Intl.NumberFormat().format(n ?? 0);
const fmtCost = (n: number | undefined): string => `$${(n ?? 0).toFixed(4)}`;

const Stat: React.FC<{ label: string; value: string }> = ({ label, value }) => (
  <div className='rounded-8px border border-solid border-border-2 bg-1 px-12px py-10px'>
    <div className='text-12px text-t-tertiary'>{label}</div>
    <div className='mt-2px text-18px font-semibold tabular-nums text-t-primary'>{value}</div>
  </div>
);

const Section: React.FC<{ title: string; children: React.ReactNode }> = ({ title, children }) => (
  <div className='pt-8px'>
    <div className='mb-6px text-13px font-medium text-t-secondary'>{title}</div>
    {children}
  </div>
);

const UsageModalContent: React.FC = () => {
  const { t } = useTranslation();
  const viewMode = useSettingsViewMode();
  const isPageMode = viewMode === 'page';
  const [days, setDays] = useState<number>(30);

  const { data, isLoading, mutate } = useSWR<UsageSummary>(
    ['crewchat-usage', days],
    () => ipcBridge.usage.summary.invoke({ days }),
    { revalidateOnFocus: false, shouldRetryOnError: false }
  );

  const totals = data?.totals;
  const hasData = (totals?.requests ?? 0) > 0;

  return (
    <div className='flex h-full flex-col'>
      <SettingsPageHeader
        title={t('settings.usage.title')}
        description={t('settings.usage.description')}
        actions={
          <div className='flex items-center gap-8px'>
            <Radio.Group type='button' size='small' value={days} onChange={(value) => setDays(Number(value))}>
              {DAY_OPTIONS.map((option) => (
                <Radio key={option} value={option}>
                  {t('settings.usage.days', { days: option })}
                </Radio>
              ))}
            </Radio.Group>
            <Button size='small' icon={<Refresh size='14' />} loading={isLoading} onClick={() => void mutate()}>
              {t('settings.usage.refresh')}
            </Button>
          </div>
        }
      />

      <AionScrollArea className='flex-1 min-h-0' disableOverflow={isPageMode}>
        <div className='px-16px pb-16px'>
          {!hasData ? (
            <EmptyState>
              <div>{t('settings.usage.empty')}</div>
              <div className='mt-4px text-12px opacity-70'>{t('settings.usage.emptyHint')}</div>
            </EmptyState>
          ) : (
            <SettingsCardList>
              <div className='grid grid-cols-2 gap-8px md:grid-cols-4'>
                <Stat label={t('settings.usage.requests')} value={fmtInt(totals?.requests)} />
                <Stat label={t('settings.usage.input')} value={fmtInt(totals?.input_tokens)} />
                <Stat label={t('settings.usage.output')} value={fmtInt(totals?.output_tokens)} />
                <Stat label={t('settings.usage.cost')} value={fmtCost(totals?.cost_usd)} />
              </div>

              <Section title={t('settings.usage.byBody')}>
                <Table data={data?.by_body ?? []} pagination={false} size='small' rowKey='body'>
                  <Table.Column title={t('settings.usage.body')} dataIndex='body' />
                  <Table.Column title={t('settings.usage.requests')} dataIndex='requests' render={fmtInt} />
                  <Table.Column title={t('settings.usage.input')} dataIndex='input_tokens' render={fmtInt} />
                  <Table.Column title={t('settings.usage.output')} dataIndex='output_tokens' render={fmtInt} />
                  <Table.Column title={t('settings.usage.cache')} dataIndex='cache_read_tokens' render={fmtInt} />
                  <Table.Column title={t('settings.usage.cost')} dataIndex='cost_usd' render={fmtCost} />
                </Table>
              </Section>

              <Section title={t('settings.usage.byModel')}>
                <Table
                  data={data?.by_model ?? []}
                  pagination={false}
                  size='small'
                  rowKey={(row) => `${row.body}:${row.model}`}
                >
                  <Table.Column title={t('settings.usage.model')} dataIndex='model' />
                  <Table.Column title={t('settings.usage.body')} dataIndex='body' />
                  <Table.Column title={t('settings.usage.requests')} dataIndex='requests' render={fmtInt} />
                  <Table.Column title={t('settings.usage.input')} dataIndex='input_tokens' render={fmtInt} />
                  <Table.Column title={t('settings.usage.output')} dataIndex='output_tokens' render={fmtInt} />
                  <Table.Column title={t('settings.usage.cost')} dataIndex='cost_usd' render={fmtCost} />
                </Table>
              </Section>

              <Section title={t('settings.usage.byDay')}>
                <Table data={data?.by_day ?? []} pagination={false} size='small' rowKey='date'>
                  <Table.Column title={t('settings.usage.date')} dataIndex='date' />
                  <Table.Column title={t('settings.usage.requests')} dataIndex='requests' render={fmtInt} />
                  <Table.Column title={t('settings.usage.input')} dataIndex='input_tokens' render={fmtInt} />
                  <Table.Column title={t('settings.usage.output')} dataIndex='output_tokens' render={fmtInt} />
                  <Table.Column title={t('settings.usage.cost')} dataIndex='cost_usd' render={fmtCost} />
                </Table>
              </Section>

              <DataLocationNote path='.data/crewchat.db' />
            </SettingsCardList>
          )}
        </div>
      </AionScrollArea>
    </div>
  );
};

export default UsageModalContent;
