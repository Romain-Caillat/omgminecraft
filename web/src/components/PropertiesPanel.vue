<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { api, type Instance, type Prop } from '../api';

const props = defineProps<{ instance: Instance; running: boolean }>();

const entries = ref<Prop[]>([]);
const managed = ref<string[]>([]);
const filter = ref('');
const newKey = ref('');
const dirty = ref(false);
const msg = ref('');
const error = ref('');

const COMMON = ['motd', 'difficulty', 'gamemode', 'hardcore', 'max-players', 'pvp', 'white-list', 'online-mode', 'view-distance', 'simulation-distance', 'spawn-protection', 'allow-flight', 'level-seed'];
const CHOICES: Record<string, string[]> = {
  difficulty: ['peaceful', 'easy', 'normal', 'hard'],
  gamemode: ['survival', 'creative', 'adventure', 'spectator'],
};

const shown = computed(() => {
  const f = filter.value.toLowerCase();
  const list = entries.value.filter((e) => !f || e.key.includes(f) || e.value.toLowerCase().includes(f));
  const rank = (k: string) => (COMMON.includes(k) ? COMMON.indexOf(k) : 1000);
  return [...list].sort((a, b) => rank(a.key) - rank(b.key) || a.key.localeCompare(b.key));
});

async function load() {
  try {
    const r = await api.props(props.instance.id);
    entries.value = r.entries;
    managed.value = r.managed;
    dirty.value = false;
  } catch (e) {
    error.value = (e as Error).message;
  }
}
onMounted(load);

function isBool(v: string) {
  return v === 'true' || v === 'false';
}
function add() {
  const k = newKey.value.trim();
  if (!k || entries.value.some((e) => e.key === k)) return;
  entries.value.push({ key: k, value: '' });
  newKey.value = '';
  dirty.value = true;
}
function remove(k: string) {
  entries.value = entries.value.filter((e) => e.key !== k);
  dirty.value = true;
}
async function save() {
  error.value = '';
  msg.value = '';
  try {
    await api.saveProps(props.instance.id, entries.value);
    msg.value = props.running ? 'Enregistré — redémarrez le serveur pour appliquer.' : 'Enregistré.';
    await load();
  } catch (e) {
    error.value = (e as Error).message;
  }
}
</script>

<template>
  <section class="panel">
    <div class="toolbar">
      <input v-model="filter" placeholder="Filtrer…" />
      <span class="spacer"></span>
      <span v-if="msg" class="ok small">{{ msg }}</span>
      <button class="primary" :disabled="!dirty" @click="save">Enregistrer</button>
    </div>
    <p v-if="error" class="err">{{ error }}</p>
    <p v-if="entries.length < 10" class="muted small">
      Le fichier complet est généré au premier démarrage du serveur ; d'ici là seules quelques clés existent.
    </p>
    <table class="props">
      <tbody>
        <tr v-for="e in shown" :key="e.key" :class="{ common: COMMON.includes(e.key) }">
          <th>{{ e.key }}</th>
          <td>
            <span v-if="managed.includes(e.key)" class="muted">{{ e.value }} (imposé par le panel)</span>
            <label v-else-if="isBool(e.value)" class="switch">
              <input
                type="checkbox"
                :checked="e.value === 'true'"
                @change="e.value = ($event.target as HTMLInputElement).checked ? 'true' : 'false'; dirty = true"
              />
              {{ e.value === 'true' ? 'oui' : 'non' }}
            </label>
            <select v-else-if="CHOICES[e.key]" v-model="e.value" @change="dirty = true">
              <option v-for="c in CHOICES[e.key]" :key="c" :value="c">{{ c }}</option>
            </select>
            <input v-else v-model="e.value" @input="dirty = true" />
          </td>
          <td class="narrow">
            <button v-if="!managed.includes(e.key)" class="icon" title="Supprimer la clé" @click="remove(e.key)">×</button>
          </td>
        </tr>
      </tbody>
    </table>
    <form class="toolbar" @submit.prevent="add">
      <input v-model="newKey" placeholder="nouvelle-cle" />
      <button>Ajouter</button>
    </form>
  </section>
</template>
