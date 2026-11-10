import { createSignal, Show } from 'solid-js';
import { AudioLines } from 'lucide-solid';
import { useAuth } from '../lib/auth';
import { Logo } from './TopBar';
import { Button, Card, Dot, Field, inputClass, Segmented } from './ui';

export function AuthScreen() {
  const auth = useAuth();
  const [mode, setMode] = createSignal<'signin' | 'signup'>('signin');
  const [name, setName] = createSignal('');
  const [email, setEmail] = createSignal('');
  const [password, setPassword] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (mode() === 'signup') await auth.signUp(name().trim(), email().trim(), password());
      else await auth.signIn(email().trim(), password());
    } catch (err) {
      console.error(err);
      setError(mode() === 'signup' ? 'Could not create the account. The email may already be registered.' : 'Email or password is wrong.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth-viewport flex min-h-dvh items-center justify-center">
      <div class="grid w-full min-w-0 max-w-4xl items-center gap-10 md:grid-cols-[1fr_minmax(0,24rem)]">
        <div class="hidden md:block">
          <Logo class="h-10 w-12" />
          <h1 class="mt-6 text-[40px] font-semibold leading-[1.08] tracking-tight">Talk through today’s lecture in ChatGPT.</h1>
          <p class="mt-4 max-w-md text-[17px] leading-relaxed text-muted">
            Add slides and notes to a course. Then ChatGPT voice explains the material, answers your questions and quizzes you from it,
            out loud.
          </p>

          {/* What a study session looks like, in the app's own card language. */}
          <Card class="mt-8 max-w-md p-5">
            <div class="flex items-start gap-3">
              <AudioLines size={20} stroke-width={1.75} class="mt-0.5 text-muted" />
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-3">
                  <h3 class="flex-1 text-[16px] font-semibold">Quiz: Eigenvalues</h3>
                  <Dot tone="running" />
                </div>
                <p class="mt-1.5 text-[15px] leading-relaxed text-muted">
                  “Take the diagonal matrix with 2 and 3. What are its eigenvalues?”
                </p>
              </div>
            </div>
          </Card>
        </div>

        <Card class="min-w-0 p-5 sm:p-7">
          <div class="mb-6 flex items-center gap-3 md:hidden">
            <Logo class="h-8 w-10" />
            <span class="text-[17px] font-semibold">TalkToBolo</span>
          </div>
          <Segmented
            label="Account"
            value={mode()}
            onChange={setMode}
            options={[
              { value: 'signin', label: 'Sign in' },
              { value: 'signup', label: 'Create account' },
            ]}
          />
          <form onSubmit={submit} class="mt-6 space-y-4">
            <Show when={mode() === 'signup'}>
              <Field label="Your name" hint="ChatGPT can greet you with it.">
                <input class={inputClass} value={name()} onInput={(e) => setName(e.currentTarget.value)} required autocomplete="given-name" />
              </Field>
            </Show>
            <Field label="Email">
              <input class={inputClass} type="email" value={email()} onInput={(e) => setEmail(e.currentTarget.value)} required autocomplete="email" autocapitalize="none" spellcheck={false} />
            </Field>
            <Field label="Password">
              <input
                class={inputClass}
                type="password"
                value={password()}
                onInput={(e) => setPassword(e.currentTarget.value)}
                required
                minLength={8}
                autocomplete={mode() === 'signup' ? 'new-password' : 'current-password'}
              />
            </Field>
            <Show when={error()}>
              <p class="text-[14px] text-red" role="alert">
                {error()}
              </p>
            </Show>
            <Button type="submit" size="lg" class="mt-2 w-full" disabled={busy()}>
              {busy() ? 'One moment…' : mode() === 'signup' ? 'Create account' : 'Sign in'}
            </Button>
          </form>
        </Card>
      </div>
    </div>
  );
}
