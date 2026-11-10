import { Route, Router, useLocation, useParams, type RouteSectionProps } from '@solidjs/router';
import { type Component, createEffect, createSignal, lazy, on, type ParentProps, Show } from 'solid-js';
import { Sp00kyProvider } from '@spooky-sync/client-solid';
import { dbConfig } from './db';
import { AuthProvider, useAuth } from './lib/auth';
import { AuthScreen } from './components/AuthScreen';
import { ChatGPTGuideProvider } from './components/ChatGPTGuide';
import { HeaderSlotProvider, useFocused } from './components/HeaderSlot';
import { RecentSessions, Sidebar } from './components/Sidebar';
import { TopBar } from './components/TopBar';
import { Loader } from './components/ui';
import Home from './routes/Home';
import CoursePage from './routes/Course';
import MaterialPage from './routes/Material';
import SessionPage from './routes/Session';
import SessionsPage from './routes/Sessions';
import SettingsPage from './routes/Settings';
import ConnectPage from './routes/Connect';
import HomeworkPage from './routes/Homework';

// three.js is large: the graph view loads it when opened.
const GraphPage = lazy(() => import('./routes/Graph'));
// So is CodeMirror, which a document's citations need from the start.
const DocumentPage = lazy(() => import('./routes/Document'));
const SitePage = lazy(() => import('./routes/Site'));

/** The app window: top bar, sidebar, content, on the gray canvas. */
function Layout(props: RouteSectionProps) {
  const auth = useAuth();
  return (
    <Show when={auth.userId()} fallback={<AuthScreen />}>
      <div class="app-viewport h-dvh">
        <div class="flex h-full flex-col overflow-hidden bg-surface md:rounded-[22px] md:border md:border-line md:shadow-frame">
          <ChatGPTGuideProvider>
            <HeaderSlotProvider>
              <Frame>{props.children}</Frame>
            </HeaderSlotProvider>
          </ChatGPTGuideProvider>
        </div>
      </div>
    </Show>
  );
}

/** Top bar, sidebars and the page. A page in focus (the graph) hides the sidebars. */
function Frame(props: ParentProps) {
  const location = useLocation();
  const focused = useFocused();
  const [menuOpen, setMenuOpen] = createSignal(false);
  createEffect(on(() => location.pathname, () => setMenuOpen(false)));

  // A new page fades in. Opacity only: a transform on <main> would become the
  // containing block of the pages' fixed buttons and shift them mid-fade.
  let main: HTMLElement | undefined;
  createEffect(
    on(
      () => [location.pathname, location.state],
      () => {
        if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;
        main?.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 200, easing: 'ease-out' });
      },
      { defer: true },
    ),
  );

  return (
    <>
      <TopBar menuOpen={menuOpen()} onMenu={() => setMenuOpen(!menuOpen())} />
      <div class="relative flex min-h-0 flex-1">
        {/* In focus the sidebars fold away (.side-fold), still mounted so their live queries stay. */}
        <div class={`side-fold ${focused() ? 'md:w-0 md:opacity-0' : 'md:w-[288px]'}`} inert={focused()}>
          <Sidebar open={menuOpen()} onClose={() => setMenuOpen(false)} />
        </div>
        <main ref={main} class="scroll-quiet min-w-0 flex-1 overflow-y-auto overscroll-contain">{props.children}</main>
        <div class={`side-fold justify-end ${focused() ? 'w-0 opacity-0' : 'xl:w-[288px]'}`} inert={focused()}>
          <RecentSessions />
        </div>
      </div>
    </>
  );
}

/**
 * A page mounted afresh for each value of a URL param, so none of its live
 * queries switches in place: sp00ky's useQuery (canary.333) writes the new
 * query's rows into the rows core still caches for the previous one, and a
 * course or lecture seen before then showed another one's details.
 */
const perParam = (name: string, Page: Component) => () => {
  const params = useParams();
  return (
    <Show when={params[name]} keyed>
      {(_key) => <Page />}
    </Show>
  );
};

export default function App() {
  return (
    <Sp00kyProvider
      config={dbConfig}
      fallback={
        <div class="flex h-dvh items-center justify-center">
          <Loader size={8} label="Opening TalkToBolo" />
        </div>
      }
      onError={(error) => console.error('Failed to start the local database', error)}
    >
      <AuthProvider>
        <Router root={Layout}>
          <Route path="/" component={Home} />
          <Route path="/courses/:courseKey" component={perParam('courseKey', CoursePage)} />
          <Route path="/courses/:courseKey/graph" component={perParam('courseKey', GraphPage)} />
          <Route path="/courses/:courseKey/materials/:materialKey" component={perParam('materialKey', MaterialPage)} />
          <Route path="/courses/:courseKey/homework/:homeworkKey" component={perParam('homeworkKey', HomeworkPage)} />
          <Route path="/courses/:courseKey/documents/:documentKey" component={perParam('documentKey', DocumentPage)} />
          <Route path="/courses/:courseKey/sites/:siteKey" component={perParam('siteKey', SitePage)} />
          {/* Study sessions live in the `call` table; links from ChatGPT use these paths. */}
          <Route path="/calls" component={SessionsPage} />
          <Route path="/calls/:callKey" component={perParam('callKey', SessionPage)} />
          <Route path="/settings" component={SettingsPage} />
          <Route path="/connect" component={ConnectPage} />
        </Router>
      </AuthProvider>
    </Sp00kyProvider>
  );
}
