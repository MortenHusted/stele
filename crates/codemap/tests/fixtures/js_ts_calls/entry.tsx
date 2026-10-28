import { importedForward, importedArrow, overload } from "@fixture/barrel";
function local() { return 1; }
const arrow = () => 2;
export function run() {
  local(); // site:lexical-function
  arrow(); // site:const-arrow
  importedForward(); // site:alias-barrel
  importedArrow(1); // site:js-arrow-import
  overload(1); // site:overload-body
  return <div onClick={local} />; // decoy:jsx-callback
}
