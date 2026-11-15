<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { api, fmtSize, type Instance, type World } from '../api';

const ask = (m: string) => window.confirm(m);
const props = defineProps<{ instance: Instance; running: boolean; liveWorld: string | null }>();

const worlds = ref<World[]>([]);
const active = ref('world');
const error = ref('');
const msg = ref('');
const progress = ref<number | null>(null);
const newName = ref('');
const newSeed = ref('');
const importName = ref('');
const input = ref<HTMLInputElement | null>(null);

async function load() {
  try {
    const r = await api.worlds(props.instance.id);
    worlds.value = r.worlds;
    active.value = r.active;
  } catch (e) {
    error.value = (e as Error).message;
  }
}
onMounted(load);

async function run(fn: () => Promise<unknown>, done?: string) {
  error.value = '';
  msg.value = '';
  try {
    await fn();
    if (done) msg.value = props.running ? `${done} Redémarrez le serveur pour basculer.` : done;
  } catch (e) {
    error.value = (e as Error).message;
  }
  await load();
}

async function importZip(files: FileList | null) {
  const f = files?.[0];
  if (!f) return;
  const form = new FormData();
  if (importName.value.trim()) form.append('name', importName.value.trim());
  form.append('file', f, f.name);
  progress.value = 0;
  await run(async () => {
    const r = await api.uploadWorld(props.instance.id, form, (p) => (progress.value = p));
    msg.value = `Monde « ${r.name} » importé.`;
  });
  progress.value = null;
  importName.value = '';
  if (input.value) input.value.value = '';
}

function createWorld() {
  const n = newName.value.trim();
  if (!n) return;
  run(() => api.createWorld(props.instance.id, n, newSeed.value), `Nouveau monde « ${n} » sélectionné, il sera généré au démarrage.`);
  newName.value = '';
  newSeed.value = '';
}
</script>

<template>
  <section class="panel">
    <p class="muted small">
      Monde chargé au démarrage : <strong>{{ active }}</strong>
      <template v-if="!worlds.some((w) => w.name === active)"> (pas encore généré)</template>
    </p>
    <p v-if="msg" class="banner info">{{ msg }}</p>
    <p v-if="error" class="err">{{ error }}</p>

    <table class="list">
      <tbody>
        <tr v-for="w in worlds" :key="w.name" :class="{ current: w.name === active }">
          <td>
            {{ w.name }}
            <span v-if="w.name === liveWorld" class="tag live">en cours</span>
            <span v-if="w.name === active" class="tag">au démarrage</span>
          </td>
          <td class="muted narrow">{{ fmtSize(w.size) }}</td>
          <td class="narrow actions">
            <button v-if="w.name !== active" @click="run(() => api.activateWorld(instance.id, w.name), `« ${w.name} » sera chargé au prochain démarrage.`)">Utiliser</button>
            <a class="button" :href="api.worldDownloadUrl(instance.id, w.name)" download>Télécharger</a>
            <button
              class="danger"
              :disabled="w.name === liveWorld"
              @click="ask(`Supprimer définitivement le monde « ${w.name} » ?`) && run(() => api.deleteWorld(instance.id, w.name))"
            >Supprimer</button>
          </td>
        </tr>
        <tr v-if="!worlds.length"><td class="muted">Aucun monde — il sera généré au premier démarrage.</td></tr>
      </tbody>
    </table>

    <div class="cards">
      <div class="card">
        <h3>Importer une map (.zip)</h3>
        <p class="muted small">L'archive doit contenir un <code>level.dat</code>, à la racine ou dans un dossier.</p>
        <input v-model="importName" placeholder="Nom du monde (facultatif)" />
        <input ref="input" type="file" accept=".zip" hidden @change="importZip(($event.target as HTMLInputElement).files)" />
        <button class="primary" :disabled="progress !== null" @click="input?.click()">
          {{ progress === null ? 'Choisir le .zip' : `Envoi… ${progress} %` }}
        </button>
      </div>
      <form class="card" @submit.prevent="createWorld">
        <h3>Nouveau monde</h3>
        <input v-model="newName" placeholder="Nom du dossier" />
        <input v-model="newSeed" placeholder="Graine (facultatif)" />
        <button class="primary" :disabled="!newName.trim()">Créer</button>
      </form>
    </div>
  </section>
</template>
