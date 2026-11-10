import { render } from 'solid-js/web';
import './styles/global.css';
// Light or dark, and following the system's switch while no choice is made.
import './lib/theme';
import App from './App';

render(() => <App />, document.getElementById('root')!);
