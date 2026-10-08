/**
 * Web-build stub for `electron`. Every call site in this fork is guarded by a
 * `window.electronAPI` check, so a browser never reaches these members.
 */
const notAvailable = (name: string) => () => {
  throw new Error(`electron.${name} is not available in the web build`);
};

export const app = {
  getPath: notAvailable('app.getPath'),
  getName: () => 'CrewChat',
  getVersion: () => '0.1.0',
  on: () => {},
  whenReady: () => Promise.resolve(),
  quit: notAvailable('app.quit')
};
export const net = { fetch: (...args: unknown[]) => fetch(...(args as [RequestInfo, RequestInit?])) };
export const Notification = undefined;
export const powerSaveBlocker = { start: () => 0, stop: () => {} };
export const utilityProcess = undefined;
export const ipcMain = { handle: () => {}, on: () => {}, removeHandler: () => {} };
export const ipcRenderer = { invoke: notAvailable('ipcRenderer.invoke'), on: () => {}, send: () => {} };
export const BrowserWindow = undefined;
export const dialog = { showOpenDialog: notAvailable('dialog.showOpenDialog') };
export const shell = { openExternal: () => Promise.resolve(), showItemInFolder: () => {} };
export const nativeTheme = { shouldUseDarkColors: false, on: () => {} };
export default {
  app,
  net,
  ipcMain,
  ipcRenderer,
  BrowserWindow,
  dialog,
  shell,
  nativeTheme
};
