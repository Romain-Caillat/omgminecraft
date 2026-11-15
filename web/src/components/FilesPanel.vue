<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, shallowRef, triggerRef } from 'vue';
import { EditorView, basicSetup } from 'codemirror';
import { EditorState, Prec, type Extension } from '@codemirror/state';
import { keymap } from '@codemirror/view';
import { indentWithTab } from '@codemirror/commands';
import { json, jsonParseLinter } from '@codemirror/lang-json';
import { yaml } from '@codemirror/lang-yaml';
import { StreamLanguage } from '@codemirror/language';
import { properties } from '@codemirror/legacy-modes/mode/properties';
import { toml } from '@codemirror/legacy-modes/mode/toml';
import { shell } from '@codemirror/legacy-modes/mode/shell';
import { linter, lintGutter } from '@codemirror/lint';
import { oneDark } from '@codemirror/theme-one-dark';
import { api, fmtSize, type FsEntry, type Instance } from '../api';

const props = defineProps<{ instance: Instance; running: boolean }>();
const id = props.instance.id;

const error = ref('');
const notice = ref('');
const conflict = ref(false);
const progress = ref<number | null>(null);

// --- arborescence --------------------------------------------------------------

const children = reactive(new Map<string, FsEntry[]>());
const expanded = reactive(new Set<string>());
const selected = ref('');

const join = (dir: string, name: string) => (dir ? `${dir}/${name}` : name);
const parentOf = (p: string) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '');
const base = (p: string) => p.slice(p.lastIndexOf('/') + 1);

interface Row {
  path: string;
  name: string;
  dir: boolean;
  depth: number;
  size: number;
}
const rows = computed(() => {
  const out: Row[] = [];
  const walk = (dir: string, depth: number) => {
    for (const e of children.get(dir) ?? []) {
      const path = join(dir, e.name);
      out.push({ path, name: e.name, dir: e.dir, depth, size: e.size });
      if (e.dir && expanded.has(path)) walk(path, depth + 1);
    }
  };
  walk('', 0);
  return out;
});

function fail(e: unknown, prefix = '') {
  error.value = prefix + (e as Error).message;
}

async function loadDir(dir: string) {
  try {
    children.set(dir, (await api.ls(id, dir)).entries);
  } catch (e) {
    children.delete(dir);
    expanded.delete(dir);
    if (dir === '') fail(e);
  }
}

async function refresh() {
  await Promise.all([...children.keys()].map(loadDir));
}

async function clickRow(r: Row) {
  selected.value = r.path;
  if (!r.dir) return open(r.path);
  if (expanded.has(r.path)) expanded.delete(r.path);
  else {
    expanded.add(r.path);
    await loadDir(r.path);
  }
}

/** Dossier visé par « nouveau fichier », « envoyer »… : la sélection, ou son parent. */
const targetDir = computed(() => {
  const r = rows.value.find((x) => x.path === selected.value);
  if (!r) return '';
  return r.dir ? r.path : parentOf(r.path);
});

// --- éditeur ---------------------------------------------------------------------

interface Tab {
  path: string;
  state: EditorState;
  mtime: number | null;
  dirty: boolean;
}
const tabs = shallowRef<Tab[]>([]);
const current = ref<string | null>(null);
const host = ref<HTMLElement | null>(null);
let view: EditorView | null = null;

const currentTab = () => tabs.value.find((t) => t.path === current.value) ?? null;
const dirtyCount = computed(() => tabs.value.filter((t) => t.dirty).length);
defineExpose({ dirtyCount });

function language(path: string): Extension[] {
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  if (ext === 'json' || ext === 'mcmeta') return [json(), linter(jsonParseLinter()), lintGutter()];
  if (ext === 'json5' || ext === 'jsonc') return [json()];
  if (ext === 'yml' || ext === 'yaml') return [yaml()];
  if (ext === 'toml') return [StreamLanguage.define(toml)];
  if (['properties', 'cfg', 'conf', 'ini', 'txt'].includes(ext)) return [StreamLanguage.define(properties)];
  if (ext === 'sh') return [StreamLanguage.define(shell)];
  return [];
}

function extensions(path: string): Extension[] {
  return [
    basicSetup,
    oneDark,
    Prec.highest(
      keymap.of([
        { key: 'Mod-s', preventDefault: true, run: () => (save(), true) },
        indentWithTab,
      ]),
    ),
    EditorView.updateListener.of((u) => {
      if (!u.docChanged) return;
      const t = currentTab();
      if (t && !t.dirty) {
        t.dirty = true;
        triggerRef(tabs);
      }
    }),
    ...language(path),
  ];
}

function stash() {
  const t = currentTab();
  if (t && view) t.state = view.state;
}

function activate(path: string) {
  stash();
  current.value = path;
  conflict.value = false;
  const t = currentTab();
  if (t && view) {
    view.setState(t.state);
    view.focus();
  }
}

async function open(path: string) {
  error.value = '';
  if (tabs.value.some((t) => t.path === path)) return activate(path);
  try {
    const r = await api.readFile(id, path);
    const tab: Tab = {
      path,
      state: EditorState.create({ doc: r.content, extensions: extensions(path) }),
      mtime: r.mtime,
      dirty: false,
    };
    tabs.value = [...tabs.value, tab];
    activate(path);
  } catch (e) {
    fail(e, `${path} : `);
  }
}

async function save() {
  const t = currentTab();
  if (!t || !view) return;
  error.value = '';
  notice.value = '';
  try {
    const r = await api.writeFile(id, t.path, view.state.doc.toString(), t.mtime);
    t.mtime = r.mtime;
    t.dirty = false;
    triggerRef(tabs);
    notice.value = props.running ? 'Enregistré — redémarrez le serveur pour appliquer.' : 'Enregistré.';
    setTimeout(() => (notice.value = ''), 4000);
    loadDir(parentOf(t.path));
  } catch (e) {
    fail(e);
    conflict.value = /modifié entre-temps/.test((e as Error).message);
  }
}

/** Relit le fichier depuis le disque, en abandonnant les modifications locales. */
async function reload() {
  const t = currentTab();
  if (!t || !view) return;
  if (t.dirty && !confirm('Abandonner vos modifications et relire le fichier ?')) return;
  try {
    const r = await api.readFile(id, t.path);
    t.state = EditorState.create({ doc: r.content, extensions: extensions(t.path) });
    t.mtime = r.mtime;
    t.dirty = false;
    view.setState(t.state);
    triggerRef(tabs);
    conflict.value = false;
    error.value = '';
  } catch (e) {
    fail(e);
  }
}

function closeTab(path: string, force = false) {
  const t = tabs.value.find((x) => x.path === path);
  if (!t) return;
  if (!force && t.dirty && !confirm(`${base(path)} n'est pas enregistré. Fermer quand même ?`)) return;
  const idx = tabs.value.indexOf(t);
  tabs.value = tabs.value.filter((x) => x !== t);
  if (current.value === path) {
    current.value = null;
    const next = tabs.value[Math.min(idx, tabs.value.length - 1)];
    if (next) activate(next.path);
  }
}

// --- actions sur les fichiers -------------------------------------------------------

async function newFile() {
  const name = prompt(`Nouveau fichier dans /${targetDir.value}`)?.trim();
  if (!name) return;
  const path = join(targetDir.value, name);
  try {
    await api.writeFile(id, path, '', null, true);
    if (targetDir.value) expanded.add(targetDir.value);
    await loadDir(targetDir.value);
    selected.value = path;
    await open(path);
  } catch (e) {
    fail(e);
  }
}

async function newFolder() {
  const name = prompt(`Nouveau dossier dans /${targetDir.value}`)?.trim();
  if (!name) return;
  try {
    await api.mkdir(id, join(targetDir.value, name));
    if (targetDir.value) expanded.add(targetDir.value);
    await loadDir(targetDir.value);
  } catch (e) {
    fail(e);
  }
}

async function rename(r: Row) {
  const name = prompt('Nouveau nom', r.name)?.trim();
  if (!name || name === r.name) return;
  const to = join(parentOf(r.path), name);
  try {
    await api.rename(id, r.path, to);
    for (const t of tabs.value) {
      if (t.path === r.path || t.path.startsWith(`${r.path}/`)) t.path = to + t.path.slice(r.path.length);
    }
    if (current.value && (current.value === r.path || current.value.startsWith(`${r.path}/`)))
      current.value = to + current.value.slice(r.path.length);
    triggerRef(tabs);
    await loadDir(parentOf(r.path));
  } catch (e) {
    fail(e);
  }
}

async function remove(r: Row) {
  if (!confirm(`Supprimer définitivement ${r.dir ? 'le dossier' : 'le fichier'} « ${r.path} » ?`)) return;
  try {
    await api.rm(id, r.path);
    for (const t of [...tabs.value]) {
      if (t.path === r.path || t.path.startsWith(`${r.path}/`)) closeTab(t.path, true);
    }
    expanded.delete(r.path);
    await loadDir(parentOf(r.path));
  } catch (e) {
    fail(e);
  }
}

const uploadInput = ref<HTMLInputElement | null>(null);
async function upload(files: FileList | null) {
  if (!files?.length) return;
  const dir = targetDir.value;
  const form = new FormData();
  for (const f of Array.from(files)) form.append('files', f, f.name);
  progress.value = 0;
  try {
    await api.uploadTo(id, dir, form, (p) => (progress.value = p));
    if (dir) expanded.add(dir);
    await loadDir(dir);
  } catch (e) {
    fail(e);
  }
  progress.value = null;
  if (uploadInput.value) uploadInput.value.value = '';
}

// --- cycle de vie ---------------------------------------------------------------------

function beforeUnload(e: BeforeUnloadEvent) {
  if (dirtyCount.value) e.preventDefault();
}

onMounted(async () => {
  view = new EditorView({ parent: host.value! });
  window.addEventListener('beforeunload', beforeUnload);
  await loadDir('');
  if (children.get('')?.some((e) => e.name === 'server.properties')) await open('server.properties');
});
onBeforeUnmount(() => {
  window.removeEventListener('beforeunload', beforeUnload);
  view?.destroy();
});

const icon = (r: Row) => (r.dir ? (expanded.has(r.path) ? '▾' : '▸') : '');
</script>

<template>
  <section class="files">
    <aside class="tree">
      <div class="tree-tools">
        <button title="Nouveau fichier" @click="newFile">+ Fichier</button>
        <button title="Nouveau dossier" @click="newFolder">+ Dossier</button>
        <button :title="`Envoyer dans /${targetDir}`" :disabled="progress !== null" @click="uploadInput?.click()">
          {{ progress === null ? '⇪' : `${progress}%` }}
        </button>
        <button title="Rafraîchir" @click="refresh">↻</button>
        <input ref="uploadInput" type="file" multiple hidden @change="upload(($event.target as HTMLInputElement).files)" />
      </div>
      <ul>
        <li
          v-for="r in rows"
          :key="r.path"
          :class="{ sel: r.path === selected, open: r.path === current, dir: r.dir }"
          :style="{ paddingLeft: `${0.5 + r.depth * 0.9}rem` }"
          :title="r.dir ? r.path : `${r.path} — ${fmtSize(r.size)}`"
          @click="clickRow(r)"
        >
          <span class="caret">{{ icon(r) }}</span>
          <span class="fname">{{ r.name }}</span>
          <span class="row-actions" @click.stop>
            <a v-if="!r.dir" :href="api.downloadUrl(id, r.path)" title="Télécharger" download>⇩</a>
            <button v-if="r.name !== 'instance.json'" title="Renommer" @click="rename(r)">✎</button>
            <button v-if="r.name !== 'instance.json'" title="Supprimer" @click="remove(r)">×</button>
          </span>
        </li>
      </ul>
    </aside>

    <div class="editor">
      <div class="editor-tabs">
        <div
          v-for="t in tabs"
          :key="t.path"
          class="etab"
          :class="{ on: t.path === current }"
          :title="t.path"
          @click="activate(t.path)"
          @mousedown.middle.prevent="closeTab(t.path)"
        >
          {{ base(t.path) }}<span v-if="t.dirty" class="dirty">●</span>
          <button class="x" @click.stop="closeTab(t.path)">×</button>
        </div>
        <span class="spacer"></span>
        <span v-if="notice" class="ok small">{{ notice }}</span>
        <button v-if="current" class="primary small-btn" :disabled="!currentTab()?.dirty" @click="save">Enregistrer</button>
      </div>
      <div v-if="error" class="banner error" @click="error = ''">
        {{ error }}
        <button v-if="conflict" class="small-btn" @click.stop="reload">Recharger depuis le disque</button>
      </div>
      <div v-show="current" ref="host" class="cm-host"></div>
      <div v-if="!current" class="editor-empty muted">
        <p>Ouvrez un fichier dans l'arborescence.</p>
        <p class="small">
          <code>server.properties</code> pour le serveur, <code>config/</code> pour les mods,
          <code>ops.json</code>, <code>whitelist.json</code>… et <code>instance.json</code> pour la mémoire,
          Java et les arguments JVM. <kbd>Ctrl</kbd>+<kbd>S</kbd> enregistre.
        </p>
      </div>
    </div>
  </section>
</template>
