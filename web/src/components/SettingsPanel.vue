<script setup lang="ts">
import { ref } from 'vue';
import { api, type Instance } from '../api';

const props = defineProps<{ instance: Instance; running: boolean }>();
const emit = defineEmits<{ changed: []; deleted: [] }>();

const name = ref(props.instance.name);
const minMb = ref(props.instance.memory_min_mb);
const maxMb = ref(props.instance.memory_max_mb);
const java = ref(props.instance.java_major);
const args = ref(props.instance.jvm_args.join(' '));
const msg = ref('');
const error = ref('');

async function save() {
  error.value = '';
  msg.value = '';
  try {
    await api.update(props.instance.id, {
      name: name.value,
      memory_min_mb: minMb.value,
      memory_max_mb: maxMb.value,
      java_major: java.value,
      jvm_args: args.value.split(/\s+/).filter(Boolean),
    });
    msg.value = props.running ? 'Enregistré — appliqué au prochain démarrage.' : 'Enregistré.';
    emit('changed');
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function del() {
  if (prompt(`Tapez « ${props.instance.id} » pour supprimer l'instance et TOUS ses mondes :`) !== props.instance.id) return;
  try {
    await api.remove(props.instance.id);
    emit('deleted');
  } catch (e) {
    error.value = (e as Error).message;
  }
}
</script>

<template>
  <section class="panel">
    <form class="settings" @submit.prevent="save">
      <label>Nom <input v-model="name" maxlength="60" /></label>
      <div class="row">
        <label>Mémoire min (Mo) <input v-model.number="minMb" type="number" min="512" step="256" /></label>
        <label>Mémoire max (Mo) <input v-model.number="maxMb" type="number" min="512" step="256" /></label>
      </div>
      <label>Version de Java
        <select v-model.number="java">
          <option v-for="j in [8, 11, 16, 17, 21, 25]" :key="j" :value="j">Java {{ j }}</option>
        </select>
        <span class="muted small">Choisie automatiquement à l'installation ; téléchargée au besoin.</span>
      </label>
      <label>Arguments JVM supplémentaires
        <input v-model="args" placeholder="-XX:+UseG1GC" />
      </label>
      <p class="muted small">
        Dossier : <code>instances/{{ instance.id }}</code> · lanceur : <code>{{ instance.jar }}</code>
      </p>
      <div class="toolbar">
        <button class="primary">Enregistrer</button>
        <span v-if="msg" class="ok small">{{ msg }}</span>
      </div>
      <p v-if="error" class="err">{{ error }}</p>
    </form>

    <div class="danger-zone">
      <h3>Zone dangereuse</h3>
      <button class="danger" :disabled="running" @click="del">Supprimer l'instance</button>
      <span v-if="running" class="muted small">Arrêtez-la d'abord.</span>
    </div>
  </section>
</template>
