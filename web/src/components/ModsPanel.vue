<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { api, fmtSize, type Instance, type ModFile } from '../api';

const ask = (m: string) => window.confirm(m);
const props = defineProps<{ instance: Instance; running: boolean }>();

const mods = ref<ModFile[]>([]);
const progress = ref<number | null>(null);
const error = ref('');
const changed = ref(false);
const input = ref<HTMLInputElement | null>(null);

async function load() {
  try {
    mods.value = (await api.mods(props.instance.id)).mods;
  } catch (e) {
    error.value = (e as Error).message;
  }
}
onMounted(load);

async function run(fn: () => Promise<unknown>) {
  error.value = '';
  try {
    await fn();
    changed.value = true;
  } catch (e) {
    error.value = (e as Error).message;
  }
  await load();
}

async function upload(files: FileList | null) {
  if (!files?.length) return;
  const form = new FormData();
  for (const f of Array.from(files)) form.append('files', f, f.name);
  progress.value = 0;
  await run(() => api.uploadMods(props.instance.id, form, (p) => (progress.value = p)));
  progress.value = null;
  if (input.value) input.value.value = '';
}

function onDrop(e: DragEvent) {
  upload(e.dataTransfer?.files ?? null);
}
</script>

<template>
  <section class="panel">
    <div class="dropzone" @dragover.prevent @drop.prevent="onDrop" @click="input?.click()">
      <input ref="input" type="file" accept=".jar" multiple hidden @change="upload(($event.target as HTMLInputElement).files)" />
      <template v-if="progress === null">Glissez des <strong>.jar</strong> ici ou cliquez pour en choisir</template>
      <template v-else>Envoi… {{ progress }} %</template>
    </div>
    <p class="muted small">
      Mods Fabric pour Minecraft {{ instance.mc_version }} — à trouver sur
      <a :href="`https://modrinth.com/mods?g=categories:fabric&v=${instance.mc_version}&e=server`" target="_blank" rel="noopener">Modrinth</a>.
      Pensez à Fabric API, requis par la plupart des mods.
    </p>
    <p v-if="changed && running" class="banner info">Redémarrez le serveur pour appliquer les changements de mods.</p>
    <p v-if="error" class="err">{{ error }}</p>
    <table class="list">
      <tbody>
        <tr v-for="m in mods" :key="m.file" :class="{ disabled: !m.enabled }">
          <td>{{ m.file.replace(/\.disabled$/, '') }}</td>
          <td class="muted narrow">{{ fmtSize(m.size) }}</td>
          <td class="narrow">
            <label class="switch">
              <input type="checkbox" :checked="m.enabled" @change="run(() => api.toggleMod(instance.id, m.file))" />
              {{ m.enabled ? 'actif' : 'désactivé' }}
            </label>
          </td>
          <td class="narrow">
            <button class="icon danger" title="Supprimer" @click="ask(`Supprimer ${m.file} ?`) && run(() => api.deleteMod(instance.id, m.file))">×</button>
          </td>
        </tr>
        <tr v-if="!mods.length"><td class="muted">Aucun mod.</td></tr>
      </tbody>
    </table>
  </section>
</template>
