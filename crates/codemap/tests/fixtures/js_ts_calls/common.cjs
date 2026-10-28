function cjsLeaf() { return 5; }
function cjsCaller() { return cjsLeaf(); } // site:cjs-local
module.exports = cjsCaller;
