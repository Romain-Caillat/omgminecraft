<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { api, fmtSize, type BackupKind, type BackupOverview } from '../api';

const props = defineProps<{ instanceId: string; exists: boolean; running: boolean }>();
const emit = defineEmits<{ restored: [] }>();

const overview = ref<BackupOverview | null>(null);
const busy = ref('');
const error = ref('');
const msg = ref('');

const group = computed(() => overview.value?.groups.find((g) => g.instance === props.instanceId) ?? null);
const backups = computed(() => group.value?.backups ?? []);

const kindLabel: Record<BackupKind, string> = {
  auto: 'auto',
  manuel: 'manuelle',
  'avant-restauration': 'avant restauration',
};

async function load() {
  try {
    overview.value = await api.backups();
  } catch (e) {
    error.value = (e as Error).message;
  }
}
onMounted(load);

async function run(label: string, fn: () => Promise<unknown>, done: string) {
  busy.value = label;
  error.value = '';
  msg.value = '';
  try {
    await fn();
    msg.value = done;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = '';
  }
  await load();
}

function backupNow() {
  run('backup', () => api.backupNow(props.instanceId), 'Sauvegarde terminée.');
}

function restore(file: string, when: string) {
  const what = props.exists
    ? `Remplacer l'instance par la sauvegarde du ${when} ?\n\nL'état actuel est sauvegardé juste avant (« avant restauration »).`
    : `Recréer l'instance « ${props.instanceId} » depuis la sauvegarde du ${when} ?`;
  if (!confirm(what)) return;
  run(
    file,
    async () => {
      await api.restoreBackup(props.instanceId, file);
      emit('restored');
    },
    'Restauration terminée.',
  );
}

function remove(file: string, when: string) {
  if (!confirm(`Supprimer définitivement la sauvegarde du ${when} ?`)) return;
  run(file, () => api.deleteBackup(props.instanceId, file), 'Sauvegarde supprimée.');
}

function fmtDate(s: number) {
  return new Date(s * 1000).toLocaleString('fr-FR', { dateStyle: 'medium', timeStyle: 'short' });
}
</script>

<template>
  <section class="panel">
    <p v-if="overview" class="muted small">
      Sauvegarde automatique chaque jour à {{ overview.hour }} h, <strong>seulement si quelqu'un a joué</strong> depuis la
      précédente. Les automatiques sont gardées {{ overview.retention_days }} jours (la plus récente toujours), les
      manuelles jusqu'à ce que vous les supprimiez.
      <template v-if="exists">
        <br />
        {{ group?.activity ? 'Des joueurs se sont connectés depuis la dernière sauvegarde : la prochaine aura lieu.' : 'Aucune connexion depuis la dernière sauvegarde.' }}
      </template>
    </p>
    <p v-if="msg" class="banner info">{{ msg }}</p>
    <p v-if="error" class="err">{{ error }}</p>

    <div v-if="exists" class="toolbar">
      <button class="primary" :disabled="!!busy" @click="backupNow">
        {{ busy === 'backup' ? 'Sauvegarde en cours…' : 'Sauvegarder maintenant' }}
      </button>
      <span v-if="running" class="muted small">Le serveur tourne : l'écriture du monde est suspendue le temps de la copie.</span>
    </div>

    <table class="list">
      <tbody>
        <tr v-for="b in backups" :key="b.file">
          <td>
            {{ fmtDate(b.created_at) }}
            <span class="tag" :class="{ live: b.kind === 'manuel' }">{{ kindLabel[b.kind] }}</span>
          </td>
          <td class="muted narrow">{{ fmtSize(b.size) }}</td>
          <td class="narrow actions">
            <button
              :disabled="!!busy || running"
              :title="running ? 'Arrêtez d\'abord le serveur' : ''"
              @click="restore(b.file, fmtDate(b.created_at))"
            >{{ busy === b.file ? 'Restauration…' : 'Restaurer' }}</button>
            <a class="button" :href="api.backupDownloadUrl(instanceId, b.file)" download>Télécharger</a>
            <button class="danger" :disabled="!!busy" @click="remove(b.file, fmtDate(b.created_at))">Supprimer</button>
          </td>
        </tr>
        <tr v-if="!backups.length"><td class="muted">Aucune sauvegarde pour l'instant.</td></tr>
      </tbody>
    </table>
  </section>
</template>
