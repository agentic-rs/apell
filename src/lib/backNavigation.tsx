import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { backgroundShell, onNativeBack } from './tauri';

interface BackAction {
  priority: number;
  run: () => void;
}

export class BackActions {
  private actions: BackAction[] = [];

  add(priority: number, run: () => void): () => void {
    const action = { priority, run };
    this.actions.push(action);
    return () => {
      this.actions = this.actions.filter((item) => item !== action);
    };
  }

  goBack(): boolean {
    let next: BackAction | undefined;
    for (const action of this.actions) {
      if (!next || action.priority >= next.priority) next = action;
    }
    if (!next) return false;
    next.run();
    return true;
  }
}

const BackContext = createContext<BackActions | null>(null);

export function BackNavigation({ children }: { children: ReactNode }) {
  const [actions] = useState(() => new BackActions());
  useEffect(() => {
    let disposed = false;
    let remove: (() => void) | undefined;
    let backgrounding = false;
    void onNativeBack((platform) => {
      if (disposed || actions.goBack() || platform !== 'android' || backgrounding) return;
      backgrounding = true;
      void backgroundShell()
        .catch(() => {})
        .finally(() => {
          backgrounding = false;
        });
    })
      .then((unlisten) => {
        if (disposed) unlisten?.();
        else remove = unlisten;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      remove?.();
    };
  }, [actions]);
  return <BackContext.Provider value={actions}>{children}</BackContext.Provider>;
}

export function useBackNavigation() {
  return useContext(BackContext);
}

export function useBackAction(priority: number, active: boolean, action: () => void) {
  const navigation = useBackNavigation();
  const current = useRef(action);
  useLayoutEffect(() => {
    current.current = action;
  });
  useLayoutEffect(() => {
    if (active) return navigation?.add(priority, () => current.current());
  }, [navigation, priority, active]);
}
