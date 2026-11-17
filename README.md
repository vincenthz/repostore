# repostore

repostore allow to clone repository locally and maintain the cache. it clones
the bare repository in a local cache directory and clone the local repository
on demand from this local cache adjusting the local clone to use the proper
remote.

when cloning a project that is already present, a fetch is done first to the local
cache and then it clone it as usual.

## Library

* async function that allow to maintain git clones, clone a new repository using the cache strategy, sync

## Binary

* list and stats for repositories cache
* sync all repositories or individual repository
* add / remove repo
* adjust remote
