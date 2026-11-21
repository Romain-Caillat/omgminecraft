export type Kind = 'vanilla' | 'paper' | 'fabric' | 'quilt' | 'forge' | 'neoforge' | 'custom';
export const MODDED: Kind[] = ['fabric', 'quilt', 'forge', 'neoforge'];

export interface MissingFile {
  file_name: string;
  url: string | null;
  dir: string;
  server: boolean;
  client: boolean;
}
export interface ModpackInfo {
  source: 'modrinth' | 'curseforge' | 'fichier';
  project_id: string | null;
  version_id: string | null;
  title: string;
  version_name: string;
  icon_url: string | null;
  missing: MissingFile[];
}
export type PackSource = 'modrinth' | 'curseforge';
export interface PackHit {
  id: string;
  title: string;
  description: string;
  icon_url: string | null;
  downloads: number;
  game_versions: string[];
  loaders: string[];
}
export interface PackVersion {
  id: string;
  name: string;
  game_versions: string[];
  loaders: string[];
  date: string;
}
export interface ShareStatus {
  client: boolean;
  files: number;
  mods: number;
  size: number;
  token: string | null;
  url: string | null;
  loader: string;
  missing: MissingFile[];
}
export type RunState = 'stopped' | 'starting' | 'running' | 'stopping';

export interface Instance {
  id: string;
  name: string;
  kind: Kind;
  mc_version: string;
  loader_version: string | null;
  jar: string;
  java_major: number;
  memory_min_mb: number;
  memory_max_mb: number;
  jvm_args: string[];
  status: 'installing' | 'ready' | 'error';
  error: string | null;
  created_at: number;
  modpack?: ModpackInfo | null;
  share_token?: string | null;
}

export interface Status {
  state: RunState;
  instance: string | null;
  started_at: number | null;
  world: string | null;
  players: string[];
  last_exit: string | null;
}

export interface GameVersion {
  id: string;
  type: string;
}
export interface ModFile {
  file: string;
  enabled: boolean;
  size: number;
}
export interface World {
  name: string;
  size: number;
}
export interface FsEntry {
  name: string;
  dir: boolean;
  size: number;
  mtime: number;
}

export type BackupKind = 'auto' | 'manuel' | 'avant-restauration';
export interface Backup {
  file: string;
  kind: BackupKind;
  size: number;
  created_at: number;
}
export interface BackupGroup {
  instance: string;
  exists: boolean;
  /** Un joueur s'est connecté depuis la dernière sauvegarde. */
  activity: boolean;
  backups: Backup[];
}
export interface BackupOverview {
  hour: number;
  retention_days: number;
  max_bytes: number;
  groups: BackupGroup[];
}

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    let msg = `${res.status} ${res.statusText}`;
    try {
      msg = (await res.json()).error ?? msg;
    } catch {
      /* corps non JSON */
    }
    throw new Error(msg);
  }
  return res.status === 204 ? (undefined as T) : res.json();
}

/** XHR plutôt que fetch : seul moyen de suivre la progression d'un gros envoi. */
function upload<T>(path: string, form: FormData, onProgress?: (pct: number) => void): Promise<T> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', `/api${path}`);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) onProgress?.(Math.round((e.loaded / e.total) * 100));
    };
    xhr.onload = () => {
      let data: { error?: string } = {};
      try {
        data = JSON.parse(xhr.responseText);
      } catch {
        /* vide */
      }
      if (xhr.status >= 200 && xhr.status < 300) resolve(data as T);
      else reject(new Error(data.error ?? `${xhr.status} ${xhr.statusText}`));
    };
    xhr.onerror = () => reject(new Error('envoi interrompu'));
    xhr.send(form);
  });
}

const enc = encodeURIComponent;
const i = (id: string) => `/instances/${enc(id)}`;

export const api = {
  instances: () => req<Instance[]>('GET', '/instances'),
  create: (body: { name: string; kind: Kind; mc_version: string; memory_max_mb: number; eula: boolean }) =>
    req<Instance>('POST', '/instances', body),
  update: (id: string, body: Partial<Pick<Instance, 'name' | 'memory_min_mb' | 'memory_max_mb' | 'jvm_args' | 'java_major'>>) =>
    req<Instance>('PATCH', i(id), body),
  remove: (id: string) => req<void>('DELETE', i(id)),
  start: (id: string) => req<void>('POST', `${i(id)}/start`),
  stop: () => req<void>('POST', '/server/stop'),
  restart: () => req<void>('POST', '/server/restart'),
  command: (command: string) => req<void>('POST', '/console', { command }),
  versions: (kind: Kind) => req<GameVersion[]>('GET', `/versions/${kind}`),

  ls: (id: string, path: string) => req<{ entries: FsEntry[] }>('GET', `${i(id)}/fs?path=${enc(path)}`),
  readFile: (id: string, path: string) => req<{ content: string; mtime: number }>('GET', `${i(id)}/fs/content?path=${enc(path)}`),
  writeFile: (id: string, path: string, content: string, mtime: number | null, create = false) =>
    req<{ mtime: number }>('PUT', `${i(id)}/fs/content`, { path, content, mtime, create }),
  mkdir: (id: string, path: string) => req<void>('POST', `${i(id)}/fs/mkdir`, { path }),
  rename: (id: string, from: string, to: string) => req<void>('POST', `${i(id)}/fs/rename`, { from, to }),
  rm: (id: string, path: string) => req<void>('DELETE', `${i(id)}/fs?path=${enc(path)}`),
  uploadTo: (id: string, dir: string, form: FormData, p?: (n: number) => void) =>
    upload(`${i(id)}/fs/upload?path=${enc(dir)}`, form, p),
  downloadUrl: (id: string, path: string) => `/api${i(id)}/fs/download?path=${enc(path)}`,

  mods: (id: string) => req<{ mods: ModFile[] }>('GET', `${i(id)}/mods`),
  uploadMods: (id: string, form: FormData, p?: (n: number) => void) => upload(`${i(id)}/mods`, form, p),
  deleteMod: (id: string, file: string) => req<void>('DELETE', `${i(id)}/mods/${enc(file)}`),
  toggleMod: (id: string, file: string) => req<void>('POST', `${i(id)}/mods/${enc(file)}/toggle`),

  worlds: (id: string) => req<{ worlds: World[]; active: string }>('GET', `${i(id)}/worlds`),
  createWorld: (id: string, name: string, seed: string) => req<void>('POST', `${i(id)}/worlds`, { name, seed }),
  activateWorld: (id: string, name: string) => req<void>('POST', `${i(id)}/worlds/${enc(name)}/activate`),
  deleteWorld: (id: string, name: string) => req<void>('DELETE', `${i(id)}/worlds/${enc(name)}`),
  uploadWorld: (id: string, form: FormData, p?: (n: number) => void) =>
    upload<{ name: string }>(`${i(id)}/worlds/upload`, form, p),
  packConfig: () => req<{ curseforge: boolean }>('GET', '/modpacks/config'),
  packSearch: (source: PackSource, q: string) =>
    req<PackHit[]>('GET', `/modpacks/search?source=${source}&q=${enc(q)}`),
  packVersions: (source: PackSource, project: string) =>
    req<PackVersion[]>('GET', `/modpacks/versions?source=${source}&project=${enc(project)}`),
  packImport: (body: {
    source: PackSource;
    project_id: string;
    version_id: string;
    name: string;
    title: string;
    icon_url: string | null;
    memory_max_mb: number;
    eula: boolean;
  }) => req<Instance>('POST', '/modpacks/import', body),
  packImportShare: (body: { code: string; name: string; memory_max_mb: number; eula: boolean }) =>
    req<Instance>('POST', '/modpacks/import-share', body),
  packUpload: (form: FormData, p?: (n: number) => void) => upload<Instance>('/modpacks/upload', form, p),
  uploadMissing: (id: string, form: FormData, p?: (n: number) => void) =>
    upload<{ added: string[]; unknown: string[] }>(`${i(id)}/modpack/missing`, form, p),
  share: (id: string) => req<ShareStatus>('GET', `${i(id)}/share`),
  shareEnable: (id: string) => req<{ token: string; url: string }>('POST', `${i(id)}/share`),
  shareDisable: (id: string) => req<void>('DELETE', `${i(id)}/share`),
  shareFromServer: (id: string) => req<{ added: number }>('POST', `${i(id)}/share/from-server`),
  backups: () => req<BackupOverview>('GET', '/backups'),
  backupNow: (id: string) => req<Backup>('POST', `${i(id)}/backups`),
  restoreBackup: (id: string, file: string) => req<void>('POST', `/backups/${enc(id)}/${enc(file)}/restore`),
  deleteBackup: (id: string, file: string) => req<void>('DELETE', `/backups/${enc(id)}/${enc(file)}`),
  backupDownloadUrl: (id: string, file: string) => `/api/backups/${enc(id)}/${enc(file)}/download`,
  worldDownloadUrl: (id: string, name: string) => `/api${i(id)}/worlds/${enc(name)}/download`,
};

export function fmtSize(n: number): string {
  if (n < 1024) return `${n} o`;
  const u = ['Ko', 'Mo', 'Go', 'To'];
  let v = n / 1024;
  let k = 0;
  while (v >= 1024 && k < u.length - 1) {
    v /= 1024;
    k++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${u[k]}`;
}

export const kindLabel: Record<Kind, string> = {
  vanilla: 'Vanilla',
  paper: 'Paper',
  fabric: 'Fabric',
  quilt: 'Quilt',
  forge: 'Forge',
  neoforge: 'NeoForge',
  custom: 'Perso',
};

export function fmtCount(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)} M`;
  if (n >= 1e3) return `${Math.round(n / 1e3)} k`;
  return String(n);
}

export const stateLabel: Record<RunState, string> = {
  stopped: 'Arrêté',
  starting: 'Démarrage…',
  running: 'En ligne',
  stopping: 'Arrêt…',
};
