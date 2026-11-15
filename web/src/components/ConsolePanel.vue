<script setup lang="ts">
import { nextTick, onMounted, ref, watch } from 'vue';
import { api, type Status } from '../api';

const props = defineProps<{ logs: string[]; status: Status; selected: string }>();

const box = ref<HTMLElement | null>(null);
const follow = ref(true);
const cmd = ref('');
const history = ref<string[]>([]);
let hpos = -1;
const error = ref('');

function onScroll() {
  const el = box.value;
  if (el) follow.value = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
}
async function scroll() {
  await nextTick();
  if (follow.value && box.value) box.value.scrollTop = box.value.scrollHeight;
}
watch(() => props.logs.length, scroll);
onMounted(scroll);

function cls(line: string) {
  if (line.startsWith('[omg]')) return 'omg';
  if (line.startsWith('> ')) return 'cmd';
  if (/\/(WARN|WARNING)\]/.test(line)) return 'warn';
  if (/\/(ERROR|FATAL)\]|Exception/.test(line)) return 'error';
  return '';
}

async function send() {
  const c = cmd.value.trim();
  if (!c) return;
  error.value = '';
  try {
    await api.command(c);
    history.value.unshift(c);
    hpos = -1;
    cmd.value = '';
    follow.value = true;
  } catch (e) {
    error.value = (e as Error).message;
  }
}
function nav(d: number) {
  hpos = Math.max(-1, Math.min(history.value.length - 1, hpos + d));
  cmd.value = hpos < 0 ? '' : history.value[hpos]!;
}
</script>

<template>
  <section class="panel console">
    <p v-if="status.instance && status.instance !== selected && status.state !== 'stopped'" class="muted small">
      La console affiche toujours le serveur en cours (« {{ status.instance }} »).
    </p>
    <div ref="box" class="log" @scroll="onScroll">
      <div v-for="(l, i) in logs" :key="i" :class="cls(l)">{{ l }}</div>
      <div v-if="!logs.length" class="muted">Rien pour l'instant.</div>
    </div>
    <div v-if="status.players.length" class="players">
      En ligne : <span v-for="p in status.players" :key="p" class="player">{{ p }}</span>
    </div>
    <form class="cmdline" @submit.prevent="send">
      <input
        v-model="cmd"
        :disabled="status.state === 'stopped'"
        :placeholder="status.state === 'stopped' ? 'Serveur arrêté' : 'Commande (say, op, gamerule, tp…)'"
        @keydown.up.prevent="nav(1)"
        @keydown.down.prevent="nav(-1)"
      />
      <button class="primary" :disabled="status.state === 'stopped'">Envoyer</button>
    </form>
    <p v-if="error" class="err small">{{ error }}</p>
  </section>
</template>
