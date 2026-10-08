/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Card list container shared by every settings page. Extracted from the six
 * copy-pasted copies of the same utility classes, so the list frame changes in
 * one place instead of six.
 */

import classNames from 'classnames';
import React from 'react';

type SettingsCardListProps = {
  children: React.ReactNode;
  className?: string;
};

const SettingsCardList: React.FC<SettingsCardListProps> = ({ children, className }) => (
  <div
    className={classNames(
      'flex flex-col gap-8px rounded-12px border border-border-2 bg-2 p-8px md:rounded-16px md:p-10px',
      className
    )}
  >
    {children}
  </div>
);

export default SettingsCardList;
