/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * Centred muted line shown when a settings list has no rows.
 */

import React from 'react';

type EmptyStateProps = {
  children: React.ReactNode;
};

const EmptyState: React.FC<EmptyStateProps> = ({ children }) => (
  <div className='block py-16px text-center text-12px text-t-secondary'>{children}</div>
);

export default EmptyState;
