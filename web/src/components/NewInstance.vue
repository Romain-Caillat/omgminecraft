<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { api, type GameVersion, type Instance, type Kind } from '../api';

const emit = defineEmits<{ close: []; created: [Instance] }>();

const name = ref('');
const kind = ref<Kind>('vanilla');
const versions = ref<GameVersion[]>([]);
const version = ref('');
const snapshots = ref(false);
const memoryGb = ref(6);
const eula = ref(false);
const loading = ref(false);
const saving = ref(false);
const error = ref('');

const shown = computed(() =>
  versions.value.filter((v) => snapshots.value || v.type === 'release'),
);

async function loadVersions() {
  loading.value = true;
  error.value = '';
  try {
    versions.value = await api.versions(kind.value);
    if (!shown.value.some((v) => v.id === version.value)) version.value = shown.value[0]?.id ?? '';
  } catch (e) {
    error.value = `Versions indisponibles : ${(e as Error).message}`;
  } finally {
    loading.value = false;
  }
}
watch(kind, loadVersions, { immediate: true });
watch(snapshots, () => {
  if (!shown.value.some((v) => v.id === version.value)) version.value = shown.value[0]?.id ?? '';
});

async function submit() {
  saving.value = true;
  error.value = '';
  try {
    const inst = await api.create({
      name: name.value.trim() || `${kind.value === 'fabric' ? 'Fabric' : 'Vanilla'} ${version.value}`,
      kind: kind.value,
      mc_version: version.value,
      memory_max_mb: Math.round(memoryGb.value * 1024),
      eula: eula.value,
    });
    emit('created', inst);
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="overlay" @click.self="emit('close')">
    <form class="dialog" @submit.prevent="submit">
      <h2>Nouvelle instance</h2>

      <label>Nom
        <input v-model="name" :placeholder="`${kind === 'fabric' ? 'Fabric' : 'Vanilla'} ${version}`" maxlength="60" />
      </label>

      <div class="field">Type
        <div class="segmented">
          <button type="button" :class="{ on: kind === 'vanilla' }" @click="kind = 'vanilla'">Vanilla</button>
          <button type="button" :class="{ on: kind === 'fabric' }" @click="kind = 'fabric'">Fabric (mods)</button>
        </div>
      </div>

      <label>Version de Minecraft
        <select v-model="version" :disabled="loading">
          <option v-for="v in shown" :key="v.id" :value="v.id">{{ v.id }}{{ v.type !== 'release' ? ` (${v.type})` : '' }}</option>
        </select>
      </label>
      <label class="check"><input v-model="snapshots" type="checkbox" /> Afficher les snapshots et anciennes versions</label>

      <label>Mémoire max : <strong>{{ memoryGb }} Go</strong>
        <input v-model.number="memoryGb" type="range" min="1" max="9" step="0.5" />
      </label>

      <label class="check">
        <input v-model="eula" type="checkbox" />
        <span>J'accepte l'<a href="https://aka.ms/MinecraftEULA" target="_blank" rel="noopener">EULA de Minecraft</a></span>
      </label>

      <p class="muted small">Le serveur et la bonne version de Java sont téléchargés en arrière-plan.</p>
      <p v-if="error" class="err">{{ error }}</p>

      <div class="dialog-actions">
        <button type="button" @click="emit('close')">Annuler</button>
        <button class="primary" :disabled="saving || !version || !eula">Créer</button>
      </div>
    </form>
  </div>
</template>
