<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { api, fmtSize, type Instance, type ShareStatus } from '../api';

const props = defineProps<{ instance: Instance }>();
const emit = defineEmits<{ changed: [] }>();

const st = ref<ShareStatus | null>(null);
const error = ref('');
const msg = ref('');
const busy = ref(false);
const copied = ref(false);
const progress = ref<number | null>(null);
const input = ref<HTMLInputElement | null>(null);

async function load() {
  try {
    st.value = await api.share(props.instance.id);
  } catch (e) {
    error.value = (e as Error).message;
  }
}
onMounted(load);

async function run(fn: () => Promise<unknown>, done = '') {
  busy.value = true;
  error.value = '';
  msg.value = '';
  try {
    await fn();
    msg.value = done;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
  await load();
}

function copy() {
  if (!st.value?.url) return;
  navigator.clipboard?.writeText(st.value.url).then(() => {
    copied.value = true;
    setTimeout(() => (copied.value = false), 1500);
  });
}

function regenerate() {
  if (st.value?.token && !confirm("Créer un nouveau lien ? L'ancien cessera de fonctionner.")) return;
  run(() => api.shareEnable(props.instance.id).then(() => emit('changed')));
}

function disable() {
  if (!confirm('Désactiver le lien ? Les joueurs ne pourront plus télécharger le pack.')) return;
  run(() => api.shareDisable(props.instance.id).then(() => emit('changed')));
}

async function uploadMissing(files: FileList | null) {
  if (!files?.length) return;
  const form = new FormData();
  for (const f of Array.from(files)) form.append('file', f, f.name);
  progress.value = 0;
  await run(async () => {
    const r = await api.uploadMissing(props.instance.id, form, (p) => (progress.value = p));
    msg.value = `${r.added.length} fichier(s) ajouté(s).` + (r.unknown.length ? ` Non reconnus : ${r.unknown.join(', ')}.` : '');
    emit('changed');
  });
  progress.value = null;
  if (input.value) input.value.value = '';
}
</script>

<template>
  <section v-if="st" class="panel">
    <p class="muted small">
      Le pack client est le dossier <code>client/</code> de l'instance (visible dans l'onglet Fichiers) : mods, configs,
      resource packs côté joueur. Les joueurs le récupèrent par un lien secret, sans CurseForge : import direct dans
      Prism Launcher, ou zip à installer à la main. {{ st.loader }}, Minecraft {{ instance.mc_version }}.
    </p>
    <p v-if="msg" class="banner info">{{ msg }}</p>
    <p v-if="error" class="err">{{ error }}</p>

    <div v-if="st.missing.length" class="card warn-card">
      <h3>{{ st.missing.length }} fichier(s) à ajouter à la main</h3>
      <p class="muted small">
        Leurs auteurs interdisent le téléchargement automatique. Téléchargez-les depuis CurseForge, puis envoyez-les ici
        tels quels : chacun est reconnu par son nom et rangé côté serveur et/ou client.
      </p>
      <ul class="missing">
        <li v-for="m in st.missing" :key="m.file_name">
          <a v-if="m.url" :href="m.url" target="_blank" rel="noopener">{{ m.file_name }}</a>
          <span v-else>{{ m.file_name }}</span>
          <span class="muted small"> · {{ m.dir }} · {{ [m.server && 'serveur', m.client && 'client'].filter(Boolean).join(' + ') }}</span>
        </li>
      </ul>
      <input ref="input" type="file" multiple hidden @change="uploadMissing(($event.target as HTMLInputElement).files)" />
      <button class="primary" :disabled="busy" @click="input?.click()">
        {{ progress === null ? 'Envoyer les fichiers' : `Envoi… ${progress} %` }}
      </button>
    </div>

    <div class="card">
      <h3>Pack client</h3>
      <template v-if="st.client">
        <p>{{ st.mods }} mods, {{ st.files }} fichiers, {{ fmtSize(st.size) }}.</p>
        <button :disabled="busy" @click="run(() => api.shareFromServer(instance.id), 'Mods du serveur ajoutés au pack client.')">
          Ajouter les mods manquants du serveur
        </button>
      </template>
      <template v-else>
        <p class="muted">Pas encore de pack client. Il est créé à l'import d'un modpack, ou à partir des mods du serveur :</p>
        <button class="primary" :disabled="busy" @click="run(() => api.shareFromServer(instance.id), 'Pack client créé.')">
          Créer depuis les mods du serveur
        </button>
        <p class="muted small">Ajoutez ensuite dans <code>client/mods</code> les mods purement client (minimap, shaders…).</p>
      </template>
    </div>

    <div v-if="st.client" class="card">
      <h3>Lien pour les joueurs</h3>
      <template v-if="st.url">
        <div class="cmdline">
          <input :value="st.url" readonly @focus="($event.target as HTMLInputElement).select()" />
          <button @click="copy">{{ copied ? 'Copié !' : 'Copier' }}</button>
        </div>
        <p class="muted small">
          À envoyer aux joueurs : la page explique l'import dans Prism. Elle n'est accessible que par le VPN (port 8081).
          Le zip est préparé au premier téléchargement, puis gardé en cache tant que le pack ne change pas.
        </p>
        <div class="toolbar">
          <a class="button" :href="st.url" target="_blank" rel="noopener">Ouvrir la page</a>
          <button :disabled="busy" @click="regenerate">Nouveau lien</button>
          <button class="danger" :disabled="busy" @click="disable">Désactiver</button>
        </div>
      </template>
      <template v-else>
        <p class="muted">Aucun lien actif.</p>
        <button class="primary" :disabled="busy" @click="regenerate">Créer le lien</button>
      </template>
    </div>
  </section>
</template>
