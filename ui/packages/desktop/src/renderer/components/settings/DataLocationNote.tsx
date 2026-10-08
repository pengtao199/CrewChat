/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * States where the page's edits are stored. Required by our rules: the user must
 * be able to tell which file they are changing, whether it can be version
 * controlled, and whether other tools share it.
 */

import { Tooltip } from '@arco-design/web-react';
import { FolderClose } from '@icon-park/react';
import React from 'react';

type DataLocationNoteProps = {
  path: string;
  /** true when the location is shared with other agent CLIs (e.g. ~/.agents) */
  shared?: boolean;
  sharedHint?: string;
};

const DataLocationNote: React.FC<DataLocationNoteProps> = ({ path, shared, sharedHint }) => {
  const line = (
    <div className='flex items-center gap-6px text-11px text-t-tertiary'>
      <FolderClose theme='outline' size='12' className='shrink-0' />
      <span className='truncate'>{path}</span>
      {shared ? <span className='shrink-0 rounded-4px bg-fill-2 px-4px'>shared</span> : null}
    </div>
  );
  return shared && sharedHint ? <Tooltip content={sharedHint}>{line}</Tooltip> : line;
};

export default DataLocationNote;
