import { forward } from "./bridge";
export function jsxCaller() { return forward(); } // site:jsx-to-ts
export function Component() { return <div />; }
export function render() { return <Component />; } // decoy:jsx-component
