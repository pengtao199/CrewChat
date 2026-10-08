/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * One row inside a SettingsCardList: leading icon, title with an optional
 * subtitle line, optional badges, and trailing actions. Matches the row used by
 * the CLI and MCP pages so all settings lists read the same.
 */

import classNames from 'classnames';
import React from 'react';

type SettingsCardRowProps = {
  icon?: React.ReactNode;
  title: React.ReactNode;
  subtitle?: React.ReactNode;
  badges?: React.ReactNode;
  actions?: React.ReactNode;
  selected?: boolean;
  disabled?: boolean;
  onClick?: () => void;
  testId?: string;
};

const SettingsCardRow: React.FC<SettingsCardRowProps> = ({
  icon,
  title,
  subtitle,
  badges,
  actions,
  selected,
  disabled,
  onClick,
  testId,
}) => {
  const stop = (event: React.MouseEvent) => event.stopPropagation();

  return (
    <div
      data-testid={testId}
      className={classNames(
        'group flex items-center justify-between gap-12px rounded-12px border border-solid px-14px py-10px transition-all duration-180',
        selected
          ? 'border-primary-6 bg-primary-1'
          : 'border-transparent bg-base hover:border-border-1 hover:bg-fill-1',
        onClick ? 'cursor-pointer' : undefined
      )}
      onClick={onClick}
    >
      <div className={classNames('flex min-w-0 flex-1 items-center gap-12px', disabled && 'opacity-50')}>
        {icon ? <span className='shrink-0 text-18px leading-none'>{icon}</span> : null}
        <div className='min-w-0 flex-1'>
          <div className='flex min-w-0 items-center gap-8px'>
            <span className='truncate text-14px font-medium text-t-primary'>{title}</span>
            {badges}
          </div>
          {subtitle ? <div className='mt-2px truncate text-11px text-t-secondary'>{subtitle}</div> : null}
        </div>
      </div>
      {actions ? (
        <div className='ms-12px flex flex-shrink-0 items-center gap-8px' onClick={stop}>
          {actions}
        </div>
      ) : null}
    </div>
  );
};

export default SettingsCardRow;
