/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 *
 * No authentication: CrewChat ships as a local desktop app (loopback only), so
 * every session is the one local user. This provider keeps the `useAuth()` shape
 * the rest of the fork imports so no consumer needs to change.
 */

import React, { createContext, useContext } from 'react';

export interface AuthUser {
  id: string;
  username: string;
  display_name?: string;
  role?: string;
  created_at?: string | null;
}

interface LoginParams {
  username: string;
  password: string;
  remember?: boolean;
}

interface LoginResult {
  success: boolean;
  message?: string;
}

interface AuthContextValue {
  ready: boolean;
  user: AuthUser | null;
  status: 'authenticated';
  login: (params: LoginParams) => Promise<LoginResult>;
  logout: () => Promise<void>;
  refresh: () => Promise<void>;
  clearAuthCache: () => void;
}

const LOCAL_USER: AuthUser = { id: 'local', username: 'crewchat', display_name: 'crewchat', role: 'admin' };

const VALUE: AuthContextValue = {
  ready: true,
  user: LOCAL_USER,
  status: 'authenticated',
  login: async () => ({ success: true }),
  logout: async () => {},
  refresh: async () => {},
  clearAuthCache: () => {},
};

const AuthContext = createContext<AuthContextValue>(VALUE);

export const AuthProvider: React.FC<React.PropsWithChildren> = ({ children }) => (
  <AuthContext.Provider value={VALUE}>{children}</AuthContext.Provider>
);

export function useAuth(): AuthContextValue {
  return useContext(AuthContext);
}
