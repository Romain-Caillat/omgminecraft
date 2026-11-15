export type Kind = 'vanilla' | 'fabric' | 'custom';
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
export interface Prop {
  key: string;
  value: string;
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

  props: (id: string) => req<{ entries: Prop[]; managed: string[] }>('GET', `${i(id)}/properties`),
  saveProps: (id: string, entries: Prop[]) => req<void>('PUT', `${i(id)}/properties`, entries),

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

export const stateLabel: Record<RunState, string> = {
  stopped: 'Arrêté',
  starting: 'Démarrage…',
  running: 'En ligne',
  stopping: 'Arrêt…',
};
