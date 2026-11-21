<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { api, kindLabel, MODDED, stateLabel, type Instance, type Status } from './api';
import NewInstance from './components/NewInstance.vue';
import ConsolePanel from './components/ConsolePanel.vue';
import FilesPanel from './components/FilesPanel.vue';
import ModsPanel from './components/ModsPanel.vue';
import WorldsPanel from './components/WorldsPanel.vue';
import SettingsPanel from './components/SettingsPanel.vue';
import BackupsPanel from './components/BackupsPanel.vue';
import SharePanel from './components/SharePanel.vue';

const status = ref<Status>({ state: 'stopped', instance: null, started_at: null, world: null, players: [], last_exit: null });
const logs = ref<string[]>([]);
const connected = ref(false);
const address = ref('');
const copied = ref(false);
const instances = ref<Instance[]>([]);
const selectedId = ref<string | null>(localStorage.getItem('omg.selected'));
const tab = ref<'console' | 'files' | 'mods' | 'worlds' | 'share' | 'backups' | 'settings'>('console');
/** Instances supprimées dont il reste des sauvegardes. */
const orphans = ref<string[]>([]);
const orphan = ref<string | null>(null);
const files = ref<{ dirtyCount: number } | null>(null);
const creating = ref(false);
const busy = ref(false);
const error = ref('');
/** Interface chargée par cet onglet, et celle que sert le panel. */
const build = ref('');
const stale = ref(false);

const selected = computed(() => instances.value.find((i) => i.id === selectedId.value) ?? null);
const runningId = computed(() => (status.value.state === 'stopped' ? null : status.value.instance));
const runningName = computed(() => instances.value.find((i) => i.id === runningId.value)?.name ?? runningId.value);

watch(selectedId, (id) => {
  if (id) localStorage.setItem('omg.selected', id);
});

/** Changer d'instance ferme l'éditeur : on prévient s'il reste du non-enregistré. */
function select(id: string) {
  orphan.value = null;
  if (id === selectedId.value) return;
  if (files.value?.dirtyCount && !confirm('Des fichiers ne sont pas enregistrés. Changer d\'instance quand même ?')) return;
  selectedId.value = id;
}

async function refresh() {
  try {
    instances.value = await api.instances();
    if (!selected.value && instances.value.length) selectedId.value = instances.value[0]!.id;
    const b = await api.backups();
    orphans.value = b.groups.filter((g) => !g.exists && g.backups.length).map((g) => g.instance);
    if (orphan.value && !orphans.value.includes(orphan.value)) orphan.value = null;
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function act(fn: () => Promise<unknown>) {
  busy.value = true;
  error.value = '';
  try {
    await fn();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

function startSelected() {
  if (!selected.value) return;
  const id = selected.value.id;
  if (runningId.value && runningId.value !== id && !confirm(`Arrêter « ${runningName.value} » et lancer « ${selected.value.name} » ?`)) return;
  tab.value = 'console';
  act(() => api.start(id));
}

function onCreated(inst: Instance) {
  creating.value = false;
  selectedId.value = inst.id;
  tab.value = 'console';
  refresh();
}

let es: EventSource | null = null;
let poll: number | undefined;

function connect() {
  es = new EventSource('/api/events');
  // Le flux se rouvre après chaque redémarrage du panel : c'est le moment de
  // vérifier si l'interface a changé (déploiement) depuis le chargement.
  es.onopen = () => {
    connected.value = true;
    fetch('/api/status')
      .then((r) => r.json())
      .then((s) => {
        if (!s.build) return;
        if (!build.value) build.value = s.build;
        else if (s.build !== build.value) stale.value = true;
      })
      .catch(() => {});
  };
  es.onerror = () => (connected.value = false);
  es.addEventListener('history', () => (logs.value = []));
  es.addEventListener('log', (e) => {
    logs.value.push((e as MessageEvent).data);
    if (logs.value.length > 3000) logs.value.splice(0, logs.value.length - 3000);
  });
  es.addEventListener('status', (e) => {
    status.value = JSON.parse((e as MessageEvent).data);
  });
}

function reload() {
  if (files.value?.dirtyCount && !confirm('Des fichiers ne sont pas enregistrés. Recharger quand même ?')) return;
  location.reload();
}

function copyAddress() {
  navigator.clipboard?.writeText(address.value).then(() => {
    copied.value = true;
    setTimeout(() => (copied.value = false), 1500);
  });
}

// Un fichier lâché à côté d'une zone de dépôt ne doit pas faire ouvrir ou
// télécharger le fichier par le navigateur (et quitter le panel).
function swallowDrop(e: DragEvent) {
  if (!(e.target as HTMLElement | null)?.closest?.('.dropzone')) e.preventDefault();
}
onMounted(() => {
  window.addEventListener('dragover', swallowDrop);
  window.addEventListener('drop', swallowDrop);
});
onUnmounted(() => {
  window.removeEventListener('dragover', swallowDrop);
  window.removeEventListener('drop', swallowDrop);
});

onMounted(() => {
  refresh();
  connect();
  fetch('/api/status')
    .then((r) => r.json())
    .then((s) => (address.value = s.address ?? ''))
    .catch(() => {});
  // Les installations se font en tâche de fond : on relit tant qu'il y en a une.
  poll = window.setInterval(() => {
    if (instances.value.some((i) => i.status === 'installing')) refresh();
  }, 2000);
});
onUnmounted(() => {
  es?.close();
  clearInterval(poll);
});

const uptime = ref('');
let tick: number | undefined;
onMounted(() => {
  tick = window.setInterval(() => {
    const s = status.value.started_at;
    if (!s) return (uptime.value = '');
    const d = Math.max(0, Math.floor(Date.now() / 1000 - s));
    const h = Math.floor(d / 3600);
    const m = Math.floor((d % 3600) / 60);
    uptime.value = h ? `${h} h ${m} min` : `${m} min`;
  }, 1000);
});
onUnmounted(() => clearInterval(tick));

const modded = computed(() => !!selected.value && MODDED.includes(selected.value.kind));
watch(modded, (m) => {
  if (!m && (tab.value === 'mods' || tab.value === 'share')) tab.value = 'console';
});
</script>

<template>
  <header class="topbar">
    <div class="brand"><span class="logo"></span>omgminecraft</div>
    <div class="server-state" :class="status.state">
      <span class="dot"></span>
      <strong>{{ stateLabel[status.state] }}</strong>
      <span v-if="runningId" class="muted">— {{ runningName }}</span>
      <span v-if="status.state === 'running'" class="muted">
        · {{ status.players.length }} joueur{{ status.players.length > 1 ? 's' : '' }}
        <template v-if="uptime"> · {{ uptime }}</template>
      </span>
      <span v-else-if="status.state === 'stopped' && status.last_exit" class="muted">· {{ status.last_exit }}</span>
    </div>
    <button v-if="address" class="address" title="Adresse à taper dans Minecraft — cliquer pour copier" @click="copyAddress">
      {{ copied ? 'Copié !' : address }}
    </button>
    <div class="actions">
      <button v-if="runningId" :disabled="busy || status.state === 'stopping'" @click="act(api.restart)">Redémarrer</button>
      <button v-if="runningId" class="danger" :disabled="busy || status.state === 'stopping'" @click="act(api.stop)">Arrêter</button>
      <span class="conn" :class="{ off: !connected }" :title="connected ? 'connecté' : 'connexion perdue'"></span>
    </div>
  </header>

  <div v-if="stale" class="banner info stale">
    Le panel a été mis à jour : rechargez la page pour utiliser la nouvelle version.
    <button class="primary" @click="reload">Recharger</button>
  </div>
  <div v-if="error" class="banner error" @click="error = ''">{{ error }}</div>

  <div class="layout">
    <aside class="sidebar">
      <button class="primary wide" @click="creating = true">+ Nouvelle instance</button>
      <ul class="instances">
        <li
          v-for="inst in instances"
          :key="inst.id"
          :class="{ active: inst.id === selectedId }"
          @click="select(inst.id)"
        >
          <div class="name">
            <span v-if="inst.id === runningId" class="dot" :class="status.state"></span>
            {{ inst.name }}
          </div>
          <div class="meta">
            <span class="tag" :class="inst.kind">{{ kindLabel[inst.kind] }}</span>
            {{ inst.mc_version }}
            <span v-if="inst.status === 'installing'" class="muted">· installation…</span>
            <span v-else-if="inst.status === 'error'" class="err">· erreur</span>
          </div>
        </li>
        <li v-if="!instances.length" class="empty">Aucune instance pour l'instant.</li>
      </ul>
      <template v-if="orphans.length">
        <p class="muted small sidebar-title">Supprimées, avec sauvegardes</p>
        <ul class="instances">
          <li v-for="o in orphans" :key="o" :class="{ active: o === orphan }" @click="orphan = o">
            <div class="name">{{ o }}</div>
            <div class="meta">restaurable</div>
          </li>
        </ul>
      </template>
    </aside>

    <main v-if="orphan" class="main">
      <div class="instance-head">
        <div>
          <h1>{{ orphan }}</h1>
          <p class="muted">Instance supprimée — ses sauvegardes permettent de la recréer.</p>
        </div>
      </div>
      <BackupsPanel :key="`o-${orphan}`" :instance-id="orphan" :exists="false" :running="false" @restored="selectedId = orphan; orphan = null; refresh()" />
    </main>
    <main v-else-if="selected" class="main">
      <div class="instance-head">
        <div>
          <h1>{{ selected.name }}</h1>
          <p class="muted">
            <template v-if="selected.modpack">{{ selected.modpack.title }} {{ selected.modpack.version_name }} · </template>
            {{ kindLabel[selected.kind] }} {{ selected.mc_version }}
            <template v-if="selected.loader_version"> · loader {{ selected.loader_version }}</template>
            · Java {{ selected.java_major || '?' }} · {{ selected.memory_max_mb }} Mo
          </p>
        </div>
        <button
          v-if="selected.id !== runningId"
          class="primary big"
          :disabled="busy || selected.status !== 'ready'"
          @click="startSelected"
        >
          {{ runningId ? 'Basculer sur cette instance' : 'Démarrer' }}
        </button>
      </div>
      <div v-if="selected.status === 'error'" class="banner error">Installation en échec : {{ selected.error }}</div>
      <div v-if="selected.status === 'installing'" class="banner info">Installation en cours — suivez-la dans la console.</div>

      <nav class="tabs">
        <button :class="{ on: tab === 'console' }" @click="tab = 'console'">Console</button>
        <button :class="{ on: tab === 'files' }" @click="tab = 'files'">Fichiers</button>
        <button v-if="modded" :class="{ on: tab === 'mods' }" @click="tab = 'mods'">Mods</button>
        <button :class="{ on: tab === 'worlds' }" @click="tab = 'worlds'">Mondes</button>
        <button v-if="modded" :class="{ on: tab === 'share' }" @click="tab = 'share'">
          Partage<span v-if="selected.modpack?.missing.length" class="badge">{{ selected.modpack.missing.length }}</span>
        </button>
        <button :class="{ on: tab === 'backups' }" @click="tab = 'backups'">Sauvegardes</button>
        <button :class="{ on: tab === 'settings' }" @click="tab = 'settings'">Réglages</button>
      </nav>

      <!-- Gardé monté : les onglets ouverts et le non-enregistré survivent aux changements d'onglet. -->
      <FilesPanel
        v-show="tab === 'files'"
        ref="files"
        :key="`f-${selected.id}`"
        :instance="selected"
        :running="selected.id === runningId"
      />
      <ConsolePanel v-if="tab === 'console'" :logs="logs" :status="status" :selected="selected.id" />
      <ModsPanel v-else-if="tab === 'mods'" :key="`m-${selected.id}`" :instance="selected" :running="selected.id === runningId" />
      <WorldsPanel
        v-else-if="tab === 'worlds'"
        :key="`w-${selected.id}`"
        :instance="selected"
        :running="selected.id === runningId"
        :live-world="selected.id === runningId ? status.world : null"
      />
      <SharePanel v-else-if="tab === 'share'" :key="`p-${selected.id}`" :instance="selected" @changed="refresh" />
      <BackupsPanel
        v-else-if="tab === 'backups'"
        :key="`b-${selected.id}`"
        :instance-id="selected.id"
        :exists="true"
        :running="selected.id === runningId"
        @restored="refresh"
      />
      <SettingsPanel
        v-else-if="tab === 'settings'"
        :key="`s-${selected.id}`"
        :instance="selected"
        :running="selected.id === runningId"
        @changed="refresh"
        @deleted="selectedId = null; refresh()"
      />
    </main>
    <main v-else class="main empty-main">
      <p>Créez une première instance pour commencer.</p>
    </main>
  </div>

  <NewInstance v-if="creating" @close="creating = false" @created="onCreated" />
</template>
