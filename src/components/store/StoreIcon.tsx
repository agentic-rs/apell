import { useEffect, useRef, useState } from 'react';
import { getStoreIcon } from '../../lib/tauri';

const images = new Map<string, string>();
const pending = new Map<string, Promise<string>>();
const queue: (() => void)[] = [];
let active = 0;

function next() {
  while (active < 3 && queue.length) queue.shift()?.();
}

function loadIcon(key: string, store_url: string, icon_url: string): Promise<string> {
  const cached = images.get(key);
  if (cached) return Promise.resolve(cached);
  const existing = pending.get(key);
  if (existing) return existing;
  const request = new Promise<string>((resolve, reject) => {
    queue.push(() => {
      active++;
      getStoreIcon(store_url, icon_url)
        .then((image) => {
          if (!image.startsWith('data:image/png;base64,') || image.length > 360_000)
            throw new Error('Invalid applet icon.');
          if (images.size >= 64) images.delete(images.keys().next().value!);
          images.set(key, image);
          resolve(image);
        })
        .catch(reject)
        .finally(() => {
          pending.delete(key);
          active--;
          next();
        });
    });
  });
  pending.set(key, request);
  next();
  return request;
}

export function StoreIcon({
  store_url,
  icon_url,
  name,
  large = false,
}: {
  store_url: string;
  icon_url: string | null;
  name: string;
  large?: boolean;
}) {
  const element = useRef<HTMLSpanElement>(null);
  const key = JSON.stringify([store_url, icon_url]);
  const [loaded, setLoaded] = useState<{ key: string; image?: string }>();
  const image = loaded?.key === key ? loaded.image : images.get(key);
  useEffect(() => {
    if (!icon_url || images.has(key)) return;
    let disposed = false;
    const load = () => {
      void loadIcon(key, store_url, icon_url)
        .then((image) => {
          if (!disposed) setLoaded({ key, image });
        })
        .catch(() => {});
    };
    let observer: IntersectionObserver | undefined;
    if (element.current && typeof IntersectionObserver !== 'undefined') {
      observer = new IntersectionObserver(
        (entries) => {
          if (entries.some((entry) => entry.isIntersecting)) {
            observer?.disconnect();
            load();
          }
        },
        { rootMargin: '120px' },
      );
      observer.observe(element.current);
    } else load();
    return () => {
      disposed = true;
      observer?.disconnect();
    };
  }, [key, store_url, icon_url]);
  return (
    <span
      ref={element}
      className={`store-icon${large ? ' store-icon-large' : ''}`}
      aria-hidden="true"
    >
      {image ? (
        <img
          src={image}
          alt=""
          onError={() => {
            if (images.get(key) === image) images.delete(key);
            setLoaded({ key });
          }}
        />
      ) : (
        Array.from(name.trim())[0]?.toLocaleUpperCase() || 'a'
      )}
    </span>
  );
}
