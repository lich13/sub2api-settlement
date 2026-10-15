const actor = document.querySelector('#actor');
const theme = document.querySelector('#theme');
const frame = document.querySelector('#settlement');
const initial = new URLSearchParams(location.search);
if (['1', '2', '3'].includes(initial.get('user_id'))) actor.value = initial.get('user_id');
if (initial.get('theme') === 'dark') theme.value = 'dark';
function entry() {
  const url = new URL('/settlement/sso/start', location.origin);
  url.search = new URLSearchParams({token: `demo-${actor.value}`, user_id: actor.value, theme: theme.value});
  return url.href;
}
function load() { frame.src = entry(); }
actor.addEventListener('change', load);
theme.addEventListener('change', load);
document.querySelector('#reload').addEventListener('click', load);
document.querySelector('#direct').addEventListener('click', () => { location.href = entry(); });
load();
