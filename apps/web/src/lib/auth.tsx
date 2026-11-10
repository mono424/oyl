import type { JSX } from 'solid-js';
import { createContext, createSignal, onCleanup, Show, useContext } from 'solid-js';
import { type GetTable, type TableModel, useDb, useQuery } from '@spooky-sync/client-solid';
import type { schema } from '../schema.gen';
import { firstRow, qMe } from './query';

type User = TableModel<GetTable<typeof schema, 'user'>>;

interface AuthContextType {
  userId: () => string | null;
  user: () => User | null;
  signIn: (email: string, password: string) => Promise<void>;
  signUp: (name: string, email: string, password: string) => Promise<void>;
  signOut: () => Promise<void>;
}

const AuthContext = createContext<AuthContextType>();

export function AuthProvider(props: { children: JSX.Element }) {
  const db = useDb<typeof schema>();
  const [userId, setUserId] = createSignal<string | null>(null);
  const [ready, setReady] = createSignal(false);
  onCleanup(
    db.auth.subscribe((uid) => {
      setUserId(uid);
      setReady(true);
    }),
  );

  const me = firstRow(
    useQuery(() => {
      const id = userId();
      return id ? qMe(db, id) : null;
    }),
  );

  const value: AuthContextType = {
    userId,
    user: () => me.data() ?? null,
    signIn: async (email, password) => {
      await db.auth.signIn('account', { email, password });
    },
    signUp: async (name, email, password) => {
      await db.auth.signUp('account', { name, email, password });
    },
    signOut: async () => {
      await db.auth.signOut();
    },
  };

  return (
    <AuthContext.Provider value={value}>
      <Show when={ready()}>{props.children}</Show>
    </AuthContext.Provider>
  );
}

export function useAuth() {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error('useAuth must be used inside AuthProvider');
  return ctx;
}
