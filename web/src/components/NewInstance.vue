<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import {
  api,
  fmtCount,
  kindLabel,
  type GameVersion,
  type Instance,
  type Kind,
  type PackHit,
  type PackSource,
  type PackVersion,
} from '../api';

const emit = defineEmits<{ close: []; created: [Instance] }>();

const mode = ref<'server' | 'modpack'>('server');
const name = ref('');
const memoryGb = ref(6);
const eula = ref(false);
const saving = ref(false);
const error = ref('');

// --- serveur nu --------------------------------------------------------------
const KINDS: { kind: Kind; hint: string }[] = [
  { kind: 'vanilla', hint: 'officiel' },
  { kind: 'paper', hint: 'plugins' },
  { kind: 'fabric', hint: 'mods' },
  { kind: 'quilt', hint: 'mods' },
  { kind: 'forge', hint: 'mods' },
  { kind: 'neoforge', hint: 'mods' },
];
const kind = ref<Kind>('vanilla');
const versions = ref<GameVersion[]>([]);
const version = ref('');
const snapshots = ref(false);
const loading = ref(false);

const shown = computed(() => versions.value.filter((v) => snapshots.value || v.type === 'release'));

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

// --- modpack -----------------------------------------------------------------
const source = ref<PackSource | 'fichier'>('modrinth');
const cfAvailable = ref(false);
const query = ref('');
const hits = ref<PackHit[]>([]);
const searching = ref(false);
const pack = ref<PackHit | null>(null);
const packVersions = ref<PackVersion[]>([]);
const packVersion = ref('');
const file = ref<File | null>(null);
const progress = ref<number | null>(null);

onMounted(async () => {
  try {
    cfAvailable.value = (await api.packConfig()).curseforge;
  } catch {
    /* sans importance */
  }
});

async function search() {
  if (source.value === 'fichier') return;
  searching.value = true;
  error.value = '';
  pack.value = null;
  try {
    hits.value = await api.packSearch(source.value, query.value.trim());
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    searching.value = false;
  }
}
watch(source, () => {
  hits.value = [];
  pack.value = null;
  if (source.value !== 'fichier') search();
});
watch(mode, (m) => {
  if (m === 'modpack' && !hits.value.length && source.value !== 'fichier') search();
});

async function choose(h: PackHit) {
  if (source.value === 'fichier') return;
  pack.value = h;
  packVersions.value = [];
  packVersion.value = '';
  error.value = '';
  try {
    packVersions.value = await api.packVersions(source.value, h.id);
    packVersion.value = packVersions.value[0]?.id ?? '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

const placeholder = computed(() => {
  if (mode.value === 'server') return `${kindLabel[kind.value]} ${version.value}`;
  if (source.value === 'fichier') return file.value?.name.replace(/\.(zip|mrpack)$/, '') ?? 'Nom du modpack';
  return pack.value?.title ?? 'Nom du modpack';
});

const canSubmit = computed(() => {
  if (saving.value || !eula.value) return false;
  if (mode.value === 'server') return !!version.value;
  if (source.value === 'fichier') return !!file.value;
  return !!pack.value && !!packVersion.value;
});

async function submit() {
  saving.value = true;
  error.value = '';
  const memory = Math.round(memoryGb.value * 1024);
  try {
    let inst: Instance;
    if (mode.value === 'server') {
      inst = await api.create({
        name: name.value.trim() || placeholder.value,
        kind: kind.value,
        mc_version: version.value,
        memory_max_mb: memory,
        eula: eula.value,
      });
    } else if (source.value === 'fichier') {
      const form = new FormData();
      form.append('name', name.value.trim());
      form.append('memory_max_mb', String(memory));
      form.append('eula', String(eula.value));
      form.append('file', file.value!, file.value!.name);
      progress.value = 0;
      inst = await api.packUpload(form, (p) => (progress.value = p));
    } else {
      inst = await api.packImport({
        source: source.value,
        project_id: pack.value!.id,
        version_id: packVersion.value,
        name: name.value.trim(),
        title: pack.value!.title,
        icon_url: pack.value!.icon_url,
        memory_max_mb: memory,
        eula: eula.value,
      });
    }
    emit('created', inst);
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    saving.value = false;
    progress.value = null;
  }
}

function fmtDate(d: string) {
  const t = Date.parse(d);
  return Number.isNaN(t) ? '' : new Date(t).toLocaleDateString('fr-FR');
}
</script>

<template>
  <div class="overlay" @click.self="emit('close')">
    <form class="dialog wide-dialog" @submit.prevent="submit">
      <h2>Nouvelle instance</h2>

      <div class="segmented">
        <button type="button" :class="{ on: mode === 'server' }" @click="mode = 'server'">Serveur</button>
        <button type="button" :class="{ on: mode === 'modpack' }" @click="mode = 'modpack'">Depuis un modpack</button>
      </div>

      <template v-if="mode === 'server'">
        <div class="field">Type
          <div class="segmented wrap">
            <button v-for="k in KINDS" :key="k.kind" type="button" :class="{ on: kind === k.kind }" @click="kind = k.kind">
              {{ kindLabel[k.kind] }} <span class="muted small">{{ k.hint }}</span>
            </button>
          </div>
        </div>
        <label>Version de Minecraft
          <select v-model="version" :disabled="loading">
            <option v-for="v in shown" :key="v.id" :value="v.id">{{ v.id }}{{ v.type !== 'release' ? ` (${v.type})` : '' }}</option>
          </select>
        </label>
        <label class="check"><input v-model="snapshots" type="checkbox" /> Afficher les snapshots et anciennes versions</label>
      </template>

      <template v-else>
        <div class="segmented">
          <button type="button" :class="{ on: source === 'modrinth' }" @click="source = 'modrinth'">Modrinth</button>
          <button
            type="button"
            :class="{ on: source === 'curseforge' }"
            :disabled="!cfAvailable"
            :title="cfAvailable ? '' : 'Pas de clé API CurseForge configurée sur le serveur'"
            @click="source = 'curseforge'"
          >CurseForge</button>
          <button type="button" :class="{ on: source === 'fichier' }" @click="source = 'fichier'">Fichier .zip / .mrpack</button>
        </div>

        <template v-if="source === 'fichier'">
          <p class="muted small">Un <code>.mrpack</code> Modrinth, ou le <code>.zip</code> d'un modpack CurseForge (celui que
            propose « Download » sur le site). Un pack CurseForge demande quand même la clé API pour récupérer ses mods.</p>
          <input type="file" accept=".zip,.mrpack" @change="file = ($event.target as HTMLInputElement).files?.[0] ?? null" />
        </template>

        <template v-else>
          <div class="cmdline">
            <input v-model="query" placeholder="Rechercher un modpack…" @keydown.enter.prevent="search" />
            <button type="button" :disabled="searching" @click="search">{{ searching ? '…' : 'Chercher' }}</button>
          </div>
          <ul v-if="!pack" class="packs">
            <li v-for="h in hits" :key="h.id" @click="choose(h)">
              <img v-if="h.icon_url" :src="h.icon_url" alt="" loading="lazy" />
              <span v-else class="noicon"></span>
              <div>
                <strong>{{ h.title }}</strong>
                <span class="muted small"> · {{ fmtCount(h.downloads) }} téléchargements</span>
                <div class="muted small clamp">{{ h.description }}</div>
                <div class="small">
                  <span v-for="l in h.loaders" :key="l" class="tag">{{ l }}</span>
                  <span class="muted">{{ h.game_versions.join(', ') }}</span>
                </div>
              </div>
            </li>
            <li v-if="!hits.length && !searching" class="muted empty">Aucun résultat.</li>
          </ul>
          <div v-else class="picked">
            <img v-if="pack.icon_url" :src="pack.icon_url" alt="" />
            <div>
              <strong>{{ pack.title }}</strong>
              <button type="button" class="link" @click="pack = null">changer</button>
              <label>Version du pack
                <select v-model="packVersion" :disabled="!packVersions.length">
                  <option v-for="v in packVersions" :key="v.id" :value="v.id">
                    {{ v.name }} — {{ v.game_versions.join(', ') }} {{ v.loaders.join(', ') }} · {{ fmtDate(v.date) }}
                  </option>
                </select>
              </label>
            </div>
          </div>
        </template>
      </template>

      <label>Nom
        <input v-model="name" :placeholder="placeholder" maxlength="60" />
      </label>

      <label>Mémoire max : <strong>{{ memoryGb }} Go</strong>
        <input v-model.number="memoryGb" type="range" min="1" max="9" step="0.5" />
      </label>

      <label class="check">
        <input v-model="eula" type="checkbox" />
        <span>J'accepte l'<a href="https://aka.ms/MinecraftEULA" target="_blank" rel="noopener">EULA de Minecraft</a></span>
      </label>

      <p class="muted small">
        {{ mode === 'server' ? 'Le serveur et la bonne version de Java sont téléchargés en arrière-plan.'
          : 'Le loader, les mods et la configuration du pack sont installés en arrière-plan ; suivez-les dans la console.' }}
      </p>
      <p v-if="error" class="err">{{ error }}</p>

      <div class="dialog-actions">
        <button type="button" @click="emit('close')">Annuler</button>
        <button class="primary" :disabled="!canSubmit">
          {{ progress !== null ? `Envoi… ${progress} %` : saving ? 'Création…' : mode === 'server' ? 'Créer' : 'Importer' }}
        </button>
      </div>
    </form>
  </div>
</template>
