# `difficulty`/`average_time`/`likes`/`completions` calculés à la volée, colonnes stockées supprimées

Le schéma verrouillé en Phase 3 (`DEC-schema`) portait `puzzles.difficulty`, `puzzles.average_time`,
`puzzles.likes` et `puzzles.completions` comme colonnes stockées, maintenues par événement (Phase 3
D-06 les laissait à leur valeur par défaut jusqu'à cette phase). La Phase 7 choisit à la place de les
calculer à la volée par agrégation (`JOIN`/`GROUP BY`) sur `puzzle_completions` à chaque `list`/
`search`/`download`, plutôt que de les maintenir en écriture à chaque complétion. Conséquence directe :
ces quatre colonnes deviennent mortes et sont supprimées par migration — un futur lecteur du schéma
qui chercherait `puzzles.likes` ne le trouvera plus, il doit savoir que la valeur vient d'un calcul
sur `puzzle_completions`. Ce choix évite tout risque de dérive entre un compteur dénormalisé et la
réalité des lignes de `puzzle_completions` (en particulier pour `likes`, togglable par re-complétion
depuis la Phase 6 D-02), au prix d'une agrégation SQL sur le chemin de lecture — jugée acceptable à
l'échelle du projet (VPS unique, communauté de niche) moyennant des index dédiés sur
`puzzle_completions(puzzle_id)` et `(puzzle_id, liked)`, plutôt qu'une vue matérialisée jugée
disproportionnée pour l'instant. `puzzles.downloads` reste seul exception : aucune table de log des
téléchargements n'existe pour le recalculer à la volée, il reste un compteur stocké incrémenté à
l'écriture.
