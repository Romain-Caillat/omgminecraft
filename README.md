# omgminecraft

Un mini « OMGServ » perso : un panel web pour gérer plusieurs serveurs Minecraft
sur une seule machine et passer de l'un à l'autre en un clic.

- **Instances** : vanilla ou Fabric, dans **n'importe quelle version** publiée par
  Mojang (snapshots et anciennes versions compris). Le jar serveur est téléchargé
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
- **Mods** (Fabric) : glisser-déposer de `.jar`, activer/désactiver, supprimer.
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

Un Java système `/usr/lib/jvm/temurin-<major>-{jre,jdk}-<arch>` est utilisé en
priorité ; sinon le JRE est téléchargé dans `java/<major>/`.

## Construire

Il faut Docker (rien d'autre) :

```bash
docker build -t omgminecraft-build .
docker run --rm omgminecraft-build > omgminecraft && chmod +x omgminecraft
```

Le build lance aussi `vue-tsc` et les tests Rust. Le binaire est statique (musl).

En développement : `OMG_DATA_DIR=./data OMG_LISTEN=127.0.0.1:8080 cargo run` d'un
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
