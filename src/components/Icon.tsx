type IconName =
  | 'add'
  | 'settings'
  | 'back'
  | 'close'
  | 'chat'
  | 'file'
  | 'more'
  | 'exit'
  | 'reload'
  | 'info'
  | 'trash'
  | 'store'
  | 'download'
  | 'account'
  | 'connections'
  | 'shield';

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
      {name === 'store' && <path d="M4 8h16l-1 13H5L4 8Zm4 1V6a4 4 0 0 1 8 0v3" />}
      {name === 'download' && <path d="M12 3v12m-4-4 4 4 4-4M4 15v5h16v-5" />}
      {name === 'account' && (
        <>
          <circle cx="12" cy="8" r="3.5" />
          <path d="M5 20v-1.5a7 7 0 0 1 14 0V20" />
        </>
      )}
      {name === 'connections' && (
        <path d="m10 14 4-4m-5 1 4-4a3.5 3.5 0 0 1 5 5l-2 2m-1-1-4 4a3.5 3.5 0 0 1-5-5l2-2" />
      )}
      {name === 'shield' && (
        <path d="M12 3c2.2 1.7 4.5 2.7 7 3.1v5c0 4.4-2.3 7.4-7 9.9-4.7-2.5-7-5.5-7-9.9v-5C7.5 5.7 9.8 4.7 12 3Zm-3 8.8 2 2 4-4" />
      )}
      {name === 'back' && <path d="m14 5-7 7 7 7" />}
      {name === 'close' && <path d="m6 6 12 12M18 6 6 18" />}
      {name === 'trash' && <path d="M3 6h18M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7" />}
      {name === 'more' && (
        <g fill="currentColor" stroke="none">
          <circle cx="5" cy="12" r="1.6" />
          <circle cx="12" cy="12" r="1.6" />
          <circle cx="19" cy="12" r="1.6" />
        </g>
      )}
      {name === 'exit' && (
        <>
          <circle cx="12" cy="12" r="8" />
          <circle cx="12" cy="12" r="3" fill="currentColor" stroke="none" />
        </>
      )}
      {name === 'reload' && <path d="M20 6v5h-5M20 11a8 8 0 1 0-2 6" />}
      {name === 'info' && (
        <>
          <circle cx="12" cy="12" r="9" />
          <path d="M12 11v6" />
          <circle cx="12" cy="7" r="1" fill="currentColor" stroke="none" />
        </>
      )}
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
