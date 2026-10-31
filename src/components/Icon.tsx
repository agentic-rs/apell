type IconName = 'add' | 'settings' | 'back' | 'close' | 'chat' | 'file';

export function Icon({ name }: { name: IconName }) {
  return (
    <svg
      width="24"
      height="24"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {name === 'add' && <path d="M12 5v14M5 12h14" />}
      {name === 'back' && <path d="m14 5-7 7 7 7" />}
      {name === 'close' && <path d="m6 6 12 12M18 6 6 18" />}
      {name === 'chat' && (
        <path d="M20 11.5a8 8 0 0 1-8 8H5l-3 2V11.5a9 9 0 0 1 18 0ZM7 10h8M7 14h5" />
      )}
      {name === 'file' && (
        <>
          <path d="M14 3H5v18h14V8l-5-5Z" />
          <path d="M14 3v5h5M8 13h8M8 17h5" />
        </>
      )}
      {name === 'settings' && (
        <>
          <path d="m9 3-.6 2.2-2 .9-2-.6-2 3.5L4 10.5v2.3l-1.6 1.6 2 3.5 2.2-.6 1.9 1L9 21h4l.6-2.2 2-.9 2 .6 2-3.5-1.6-1.6v-2.3L19.6 9l-2-3.5-2.2.6-1.9-1L13 3H9Z" />
          <circle cx="11" cy="12" r="3" />
        </>
      )}
    </svg>
  );
}
