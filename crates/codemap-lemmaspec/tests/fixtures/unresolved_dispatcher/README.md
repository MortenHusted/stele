This synthetic program reaches `service::handle` through a function parameter.
The test supplies one exact call (`main` to `dispatch`) and one unresolved
call (`callback`), without inventing a call edge to `handle`. Under those
observations, both absence checkers must reject default coverage admission.
A lower admission threshold permits only a conditional observed-graph finding;
it does not prove that `handle` cannot run. Test observations are hand-built,
not recorded output from a language server.
