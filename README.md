# omgminecraft

Un mini « OMGServ » perso : un panel web pour gérer plusieurs serveurs Minecraft
sur une seule machine et passer de l'un à l'autre en un clic.

- **Instances** : Vanilla, Paper, Fabric, Quilt, Forge ou NeoForge, dans
  **n'importe quelle version** que le projet publie (snapshots et anciennes versions
  compris). Le jar serveur est téléchargé
  et vérifié (SHA-1), et la **bonne version de Java** est installée automatiquement
  (Temurin, via Adoptium) : Java 8 pour une 1.12, 17 pour une 1.20.1, 21, 25…
- **Pas de whitelist** : `white-list` et `enforce-whitelist` sont forcés à `false`
  et les fichiers de listes sont masqués — qui peut jouer, c'est NetBird.
- **Une seule instance tourne à la fois**, sur le port 25565 : « Basculer sur cette
  instance » arrête proprement la courante (`stop`, sauvegarde) et lance l'autre.
- **Console** en direct (SSE), envoi de commandes, joueurs connectés.
- **Fichiers** : explorateur + éditeur façon VS Code (CodeMirror, onglets, Ctrl+S,
  coloration JSON/TOML/YAML/properties, erreurs JSON soulignées). Toute la config
  passe par là : `server.properties`, `config/` des mods, `ops.json`, `instance.json`
  (validé avant écriture)… Si le serveur a réécrit un fichier depuis son ouverture,
  l'enregistrement est refusé au lieu d'écraser ses changements.
- **Mods** (Fabric, Quilt, Forge, NeoForge) : glisser-déposer de `.jar`,
  activer/désactiver, supprimer.
- **Modpacks** : recherche et import depuis **Modrinth** ou **CurseForge**, ou envoi
  d'un `.mrpack` / `.zip`. Le loader et sa version exacte, les mods côté serveur et
  la configuration du pack sont installés tout seuls.
- **Partage du pack client** : un lien secret donne aux joueurs une page avec
  l'instance **Prism Launcher** prête à importer (mods, configs, serveur déjà dans
  la liste multijoueur) et un zip brut. Pas besoin de CurseForge côté joueur.
- **Sauvegardes** automatiques et manuelles (voir plus bas).
- **Mondes** : plusieurs mondes par instance, choix du monde chargé, import d'une
  map en `.zip`, nouveau monde avec graine, téléchargement d'une sauvegarde
  (`save-off` + `save-all flush` pendant l'archivage si le monde tourne).
- **Redémarrage du panel** : l'instance qui tournait est relancée automatiquement.

Rust (axum) + Vue 3. Le front est embarqué dans le binaire : un seul fichier
statique à déployer, aucune dépendance sur la machine cible hors Java (et encore :
il se télécharge tout seul).

## Accès et sécurité

Le panel **n'a pas de comptes** : l'authentification, c'est NetBird. Deux verrous :

1. **Dans l'application** : toute requête dont l'IP source n'est pas dans
   `OMG_ALLOWED_CIDRS` (par défaut NetBird `100.64.0.0/10` + boucle locale) reçoit
   un 403. Le LAN n'y a donc **pas** accès, même s'il atteint le port.
2. **Dans NetBird** : seuls les groupes autorisés par une policy atteignent le port
   du panel. Les joueurs (`team-minecraft`) n'ont que `25565/tcp` ; le panel est
   réservé à un groupe dédié.

Le panel peut lancer n'importe quel jar et exécuter des commandes serveur :
quiconque y accède contrôle le compte `minecraft` de la machine. Ne l'ouvrez
qu'à des gens de confiance, et jamais via le reverse proxy public.

### NetBird (modèle srv-* / team-*)

Le panel tourne sur la machine déjà enrôlée dans `srv-minecraft`, il n'y a donc
pas d'agent à ajouter :

- groupe de personnes **`team-minecraft-admin`** (dans les `auto_groups` des
  utilisateurs concernés) ;
- **une** policy, une règle : `team-minecraft-admin` → `srv-minecraft`, `8080/tcp`.

Le groupe `admin` atteint déjà tous les ports de `srv-minecraft`. Adresse :
**http://minecraft.netbird.selfhosted:8080**.

## Configuration

| Variable | Défaut | Rôle |
|---|---|---|
| `OMG_DATA_DIR` | `/srv/minecraft` | Racine : `instances/`, `java/`, `tmp/`, `omg.json` |
| `OMG_LISTEN` | `0.0.0.0:8080` | Adresse d'écoute du panel |
| `OMG_ALLOWED_CIDRS` | `100.64.0.0/10,127.0.0.0/8,::1/128` | Réseaux autorisés |
| `OMG_GAME_PORT` | `25565` | Port imposé à chaque instance (`server-port`) |
| `OMG_DEFAULT_MEMORY_MB` | `6144` | Mémoire max proposée à la création |
| `OMG_BACKUP_DIR` | `$OMG_DATA_DIR/backups` | Sauvegardes — à monter **à part** des données |
| `OMG_BACKUP_HOUR` | `4` | Heure locale de la sauvegarde automatique |
| `OMG_BACKUP_RETENTION_DAYS` | `14` | Conservation des sauvegardes automatiques |
| `OMG_BACKUP_MAX_GB` | `3` | Place maximale des sauvegardes d'une instance |
| `OMG_CURSEFORGE_API_KEY` | — | Clé API CurseForge ; sans elle, Modrinth et l'import de fichier seulement |
| `OMG_SHARE_LISTEN` | `0.0.0.0:8081` | Surface joueurs : téléchargement des packs clients |
| `OMG_SHARE_URL` | `http://localhost:8081` | Base des liens donnés aux joueurs |

Arborescence :

```
/srv/minecraft/
├── omg.json                  instance active + « doit tourner »
├── java/<major>/             JRE téléchargés (si absents du système)
├── tmp/                      téléchargements et archives en cours
└── instances/<id>/
    ├── instance.json         type, version, Java, mémoire, args JVM
    ├── server.jar / fabric-server-launch.jar
    ├── server.properties, eula.txt, mods/, world/, …
```

Sur ce serveur, `/srv/minecraft` est **`data/` à la racine de ce dépôt**, monté
dans PCT 107 par `mp0: /mnt/storage/omgminecraft/data,mp=/srv/minecraft`. Tous les
serveurs, mondes et JRE y sont. Il est ignoré par git et par Docker, mais un
`git clean -fdx`, un `rm -rf` du clone ou un re-clone **efface tous les mondes** :
ne jamais faire ça ici. Les snapshots ZFS quotidiens de `media_disk/docker`
(`/mnt/storage/.zfs/snapshot/`) sont la seule sauvegarde.

## Loaders

| Type | Installation | Lancement |
|---|---|---|
| Vanilla | jar Mojang (SHA-1 vérifié) | `-jar server.jar` |
| Paper | dernier build de fill.papermc.io | `-jar server.jar` |
| Fabric | lanceur serveur de meta.fabricmc.net | `-jar fabric-server-launch.jar` |
| Quilt | `quilt-installer install server … --download-server` | `-jar quilt-server-launch.jar` |
| Forge | installeur officiel `--installServer` (version recommandée, sinon la dernière) | `@libraries/…/unix_args.txt` |
| NeoForge | installeur officiel `--installServer` (dernière stable, sinon bêta) | `@libraries/…/unix_args.txt` |

`jar` dans `instance.json` est la cible de lancement : un `.txt` est passé en
fichier d'arguments (`@…`), sinon en `-jar`. Pour Forge, NeoForge et Quilt,
`libraries/` est posé par l'installeur et fait donc partie des sauvegardes.

## Modpacks et partage

**Import.** Onglet « Depuis un modpack » de la création d'instance. Le pack fixe la
version de Minecraft, le loader et sa version. Chaque fichier va côté serveur
(dossier de l'instance), côté client (`client/`), ou des deux (un seul fichier
sur disque, lien physique) :

- Modrinth : d'après le champ `env` de chaque fichier ; `overrides/` des deux
  côtés (sauf resource packs, shaders, `options.txt`… côté serveur),
  `server-overrides/` et `client-overrides/` d'un seul.
- CurseForge : les mods marqués « Client » ou « Server » vont d'un seul côté, les
  resource packs et shaders côté client. **Certains auteurs interdisent le
  téléchargement par des outils tiers** : ces fichiers n'ont pas d'URL. Ils sont
  listés dans l'onglet **Partage** avec le lien de leur page ; on les télécharge à
  la main et on les envoie tels quels, ils sont reconnus par leur nom.

**CurseForge.** Certaines clés API n'ont pas accès à la recherche (`/mods/search`
→ 403) alors que tout le reste répond. Le panel se rabat alors sur les modpacks
populaires (`/mods/featured`) pour une recherche vide, et accepte l'**ID du
projet** (page du modpack, « Project ID ») ou un lien `…/projects/<id>`. Un
**profil partagé depuis l'app CurseForge** (`curseforge.com/minecraft/share/<code>`)
s'importe directement : `/v1/shared-profile/<code>` renvoie un zip de modpack
ordinaire.

Les packs déclarent souvent mal leurs mods purement client. Forge/NeoForge refuse
alors de démarrer (« invalid dist DEDICATED_SERVER ») ; le panel lit le rapport de
crash et nomme dans la console les jars à désactiver dans l'onglet Mods.

**Partage.** Onglet **Partage** des instances moddées. Le pack client est le
dossier `client/` (contenu d'un `.minecraft`) : rempli par l'import, ou créé depuis
les mods du serveur (liens physiques + copie de `config/`), à compléter à la main
par les mods purement client. Un lien `OMG_SHARE_URL/p/<jeton>` (48 caractères
hex, révocable, régénérable) ouvre sur `OMG_SHARE_LISTEN` une page avec :

- `prism.zip` : instance Prism Launcher (`instance.cfg`, `mmc-pack.json`,
  `.minecraft/`), importable directement par URL ; Prism résout lui-même les
  dépendances du loader (LWJGL, intermediary) ;
- `mods.zip` : le contenu brut et un `LISEZMOI.txt`.

Un `servers.dat` contenant `OMG_PUBLIC_ADDRESS` est ajouté si le pack n'en a pas.
Les zips sont construits au premier téléchargement et gardés dans
`.share-cache/` (exclu des sauvegardes) tant que le pack ne change pas.

Ce port ne sert que ces trois routes : rien de l'API d'administration n'y est
accessible. Sur PCT 107 il est ouvert à `team-minecraft` par une policy NetBird
dédiée (8081/tcp).

## Sauvegardes

Onglet **Sauvegardes** de chaque instance : sauvegarder maintenant, restaurer,
télécharger, supprimer. Une archive `<instance>/<date>-<type>.tar.gz` contient
l'instance entière (`instance.json`, jar, réglages, mods, mondes), sauf
`libraries/`, `versions/`, `.fabric/` et `logs/` que le serveur régénère.

- **auto** : chaque jour à `OMG_BACKUP_HOUR`, ou au premier passage après si le
  panel était éteint, **seulement pour les instances où un joueur s'est connecté
  depuis la sauvegarde précédente** (suivi dans `state.json`). Gardées
  `OMG_BACKUP_RETENTION_DAYS` jours ; la plus récente de chaque instance n'est
  jamais supprimée, même vieille.
- **manuel** : gardées jusqu'à suppression.
- **avant-restauration** : prise automatiquement avant chaque restauration,
  même durée de vie que les auto.

Au-delà de `OMG_BACKUP_MAX_GB` (3 Go) par instance, les sauvegardes les plus
anciennes sont supprimées après chaque nouvelle : auto et avant-restauration
d'abord, manuelles en dernier recours, jamais la plus récente. Ce plafond
l'emporte sur la durée de conservation.

Si le serveur tourne, la copie se fait entre `save-off` / `save-all flush` et
`save-on` : l'état est cohérent et les joueurs ne sont pas déconnectés. Une
restauration exige l'instance arrêtée. Les sauvegardes d'une instance
supprimée restent, et la barre latérale permet de la recréer.

Sur PCT 107, elles vivent dans `/mnt/storage/omgminecraft/backups`, monté par
`mp1` sur `/srv/backups` : un effacement de `data/` ne les emporte pas.

Un Java système `/usr/lib/jvm/temurin-<major>-{jre,jdk}-<arch>` est utilisé en
priorité ; sinon le JRE est téléchargé dans `java/<major>/`.

## Construire

Il faut Docker (rien d'autre) :

```bash
docker build -t omgminecraft-build .
docker run --rm omgminecraft-build > omgminecraft && chmod +x omgminecraft
```

Le build lance aussi `vue-tsc` et les tests Rust. Le binaire est statique (musl).

En développement : `OMG_DATA_DIR=./dev-data OMG_LISTEN=127.0.0.1:8080 cargo run` d'un
côté (après un premier `npm run build` dans `web/`, le binaire embarque `web/dist`),
`npm run dev` dans `web/` de l'autre.

## Déployer (PCT 107)

```bash
# depuis l'hôte Proxmox
pct push 107 omgminecraft /usr/local/bin/omgminecraft --perms 755
pct push 107 deploy/omgminecraft.service /etc/systemd/system/omgminecraft.service
pct exec 107 -- systemctl daemon-reload
pct exec 107 -- systemctl enable --now omgminecraft
```

Le panel remplace `minecraft.service` : c'est lui qui lance Java. Les deux ne
doivent jamais tourner ensemble (même port).

### Reprendre un serveur existant

Une instance n'est qu'un dossier avec un `instance.json`. Pour adopter le serveur
déjà en place (exemple : One Life, Fabric 26.3) :

```bash
pct exec 107 -- systemctl disable --now minecraft.service minecraft.socket
pct exec 107 -- sh -c '
  cd /srv/minecraft && mkdir -p instances/one-life &&
  for f in *; do [ "$f" = instances ] || mv "$f" instances/one-life/; done'
pct push 107 deploy/one-life.instance.json /srv/minecraft/instances/one-life/instance.json
pct exec 107 -- chown -R minecraft:minecraft /srv/minecraft
pct exec 107 -- systemctl enable --now omgminecraft
```

Puis « Démarrer » sur One Life dans le panel.

## Notes

- `-Dlog4j2.formatMsgNoLookups=true` est toujours passé à Java (Log4Shell). Il
  suffit en 1.17–1.18 ; **les versions 1.7 à 1.16.5 restent vulnérables** sans le
  fichier de configuration log4j fourni par Mojang. Le risque est limité aux
  joueurs qui atteignent le serveur — donc à vos utilisateurs NetBird.
- Le panel arrête Minecraft par `stop` et attend 60 s avant de tuer le processus.
- Un crash du serveur n'est pas relancé automatiquement : il apparaît dans la
  barre d'état (« planté (exit status: 1) ») et dans la console.
