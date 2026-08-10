-- REQ-moderation, ROADMAP SC5 (« le filtre de titres — `profane-title`, liste configurable EN/FR —
-- fonctionne ») : remplace la liste de six mots codée en dur dans `src/validation.rs`
-- (`PROFANE_WORDS`, TODO(Phase 7)) par une vraie table, modifiable sans redéploiement ni
-- redémarrage (07-08-PLAN.md `<interfaces>`).
--
-- SPEC §4.6 : « un filtre naïf suffit au lancement, l'objectif est de bloquer l'évident, la
-- modération humaine gère le reste » -- ce semis n'a donc pas vocation à être exhaustif.
--
-- `word` est la clé primaire de la table : le doublon est structurellement impossible (pas de
-- `UNIQUE` séparé nécessaire), et la future CLI (`savez mod profanity add|remove|list`, plan
-- 07-09) peut écrire avec un simple `ON CONFLICT (word) DO NOTHING`, sans `SELECT` préalable --
-- même discipline de contrainte-source-de-vérité qu'`insert_user`/`insert_puzzle`
-- (`src/repository.rs`).
--
-- `lang` est contraint par un `CHECK` à `{en, fr}` : les deux langues couvertes par ce semis, et
-- les deux seules valeurs que `src/profanity.rs::add_word` accepte en écriture.
--
-- Contenu du semis : entrées d'un seul mot, en minuscules, sans espace ni ponctuation (le filtre
-- de `src/profanity.rs::ProfanityList::contains_token` travaille par jeton, jamais par
-- sous-chaîne). Source : une sélection restreinte aux entrées d'un seul mot des listes ouvertes de
-- référence `LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words`, fichiers `en` et
-- `fr`. Les six mots déjà présents dans l'ancien `PROFANE_WORDS` codé en dur (`fuck`, `shit`,
-- `bitch`, `merde`, `putain`, `connard`) figurent tous ci-dessous, faute de quoi des tests
-- existants de `validate_title` régresseraient.
CREATE TABLE profanity_words (
    word     TEXT PRIMARY KEY,
    lang     TEXT NOT NULL CHECK (lang IN ('en', 'fr')),
    added_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO profanity_words (word, lang) VALUES
    ('fuck', 'en'),
    ('shit', 'en'),
    ('bitch', 'en'),
    ('asshole', 'en'),
    ('bastard', 'en'),
    ('cunt', 'en'),
    ('dick', 'en'),
    ('pussy', 'en'),
    ('whore', 'en'),
    ('slut', 'en'),
    ('nigger', 'en'),
    ('faggot', 'en'),
    ('retard', 'en'),
    ('cock', 'en'),
    ('dildo', 'en'),
    ('douche', 'en'),
    ('dyke', 'en'),
    ('fag', 'en'),
    ('hoe', 'en'),
    ('jerk', 'en'),
    ('moron', 'en'),
    ('prick', 'en'),
    ('rapist', 'en'),
    ('screw', 'en'),
    ('tits', 'en'),
    ('twat', 'en'),
    ('wanker', 'en'),
    ('bollocks', 'en'),
    ('bugger', 'en'),
    ('crap', 'en'),
    ('merde', 'fr'),
    ('putain', 'fr'),
    ('connard', 'fr'),
    ('connasse', 'fr'),
    ('salope', 'fr'),
    ('pute', 'fr'),
    ('bordel', 'fr'),
    ('encule', 'fr'),
    ('connerie', 'fr'),
    ('batard', 'fr'),
    ('couillon', 'fr'),
    ('cretin', 'fr'),
    ('imbecile', 'fr'),
    ('con', 'fr'),
    ('conne', 'fr'),
    ('foutre', 'fr'),
    ('chier', 'fr'),
    ('cul', 'fr'),
    ('bite', 'fr'),
    ('couille', 'fr'),
    ('couilles', 'fr'),
    ('nichons', 'fr'),
    ('salaud', 'fr'),
    ('salopard', 'fr'),
    ('pede', 'fr'),
    ('negro', 'fr'),
    ('tantouze', 'fr'),
    ('gouine', 'fr'),
    ('enfoire', 'fr')
ON CONFLICT (word) DO NOTHING;
