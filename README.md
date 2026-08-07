# Savez

Backend Rust auto-hébergé et open source pour le mode puzzle communautaire de shapez 1, avec un objectif de conservation.

> **Statut : Draft.** L'ensemble du projet est conditionné à l'accord de tobspr, sollicité par mail le 6 août 2026, sur trois principes : validation de possession via l'API officielle, export par les créateurs de leurs propres puzzles uniquement, et coopération future sur la préservation du catalogue. Le développement se poursuit en attendant la réponse ; le périmètre pourra être révisé selon l'issue de cet échange.

## Objectifs

**Objectif principal :** pérenniser l'expérience puzzle communautaire de shapez 1 via un backend auto-hébergé, open source, indépendant de l'infrastructure officielle (`api.shapez.io`).

**Objectifs secondaires :**

- Offrir aux créateurs un outil de portabilité de leurs propres puzzles depuis le service officiel.
- Ne pas nuire au modèle commercial du DLC Puzzle tant que le service officiel est en vie (accès réservé aux possesseurs du DLC).
- Servir de projet d'apprentissage Rust pour l'auteur.

## Non-objectifs

- Copier le catalogue communautaire officiel (aucun scraping de contenu tiers).
- Se substituer au service officiel tant qu'il fonctionne.
- Version web du client (alignement sur la Community Edition : standalone uniquement).

## Statut du développement

Le projet en est à la **phase 1 sur 12** (fondations projet). Voir `docs/cahier-des-charges.md` pour la spécification complète et `.planning/ROADMAP.md` pour la feuille de route détaillée.

## Licence

Ce backend est distribué sous licence **AGPL-3.0** (voir `LICENSE`) : toute personne
hébergeant une version modifiée de ce serveur doit publier ses sources modifiées.

> À noter : si du code est repris du projet [gatez-backend](https://github.com/armandosneto/gatez-backend)
> (MIT, copyright Armando Soares e Silva Neto et Rafael Nunes Santana), le fichier
> `LICENSE-MIT` correspondant sera ajouté à ce dépôt au moment de la reprise, conformément
> aux termes de la licence MIT d'origine. À ce jour, aucun code n'a été repris de ce projet.

Le futur mod client (compatible avec le jeu [shapez 1](https://github.com/tobspr-games/shapez.io)) sera quant à lui distribué sous **GPLv3**, en tant qu'œuvre dérivée du client GPL du jeu, conformément à la section 3.1 du cahier des charges.

## Auteur

Maxime Mainguet ([Pimak](https://github.com/Pimak)).

## Références

- [tobspr-games/shapez.io](https://github.com/tobspr-games/shapez.io) — client officiel shapez 1, contrat d'API et types de données ciblés par ce backend.
- [armandosneto/gatez-backend](https://github.com/armandosneto/gatez-backend) — backend communautaire de référence (MIT), schéma de données et logique métier transposés.
