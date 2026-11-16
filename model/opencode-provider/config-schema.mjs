// Presentation of the adapter's existing configuration; no runtime defaults.
const connection = {
    id: 'connection', title: 'Подключение',
    description: 'Если явный ключ пустой, используется сохранённый ключ выбранного провайдера в OpenCode, затем его переменные окружения. Здесь поддержаны API-ключи; OAuth не поддержан. Наличие записи не подтверждает авторизацию.',
};
const catalog = {
    id: 'catalog', title: 'Каталог моделей',
    description: 'Источник метаданных OpenCode. Модель и effort задаются отдельно от подключения.',
};
const textField = (key, title, description, section, { required = false, advanced = true, secret = false } = {}) => ({
    key, title, description, section, value: { type: 'string', multiline: false, secret },
    default: null, required, advanced, unit: null,
});

export const configSchema = { fields: [
    textField('provider', 'Маршрут OpenCode',
        'ID провайдера из каталога OpenCode, например opencode-go для Go или opencode для Zen. Используется для каталога, авторизации и запросов.',
        connection, { required: true, advanced: false }),
    textField('api_key', 'Ключ API',
        'Необязательный явный ключ только для этого подключения. Если пусто — используется авторизация OpenCode или окружение. Значение хранится в конфиге Proteus; хранилище OpenCode не меняется.',
        connection, { advanced: false, secret: true }),
    textField('database_path', 'База авторизации OpenCode',
        'Необязательный путь к OpenCode v2 DB. Без значения используется opencode/opencode.db в абсолютном XDG_DATA_HOME, иначе ~/.local/share/opencode/opencode.db. Поддержаны ~/, $HOME/ и ${HOME}/. База открывается только для чтения.',
        connection),
    textField('models_file', 'Локальный каталог моделей',
        'Необязательный путь к native ModelsDev metadata файлу. При заданном файле HTTP-обновление каталога не выполняется.',
        catalog),
    textField('models_url', 'Адрес каталога моделей',
        'Необязательный адрес native ModelsDev metadata endpoint. Не меняет адрес API модели; при заданном models_file HTTP-обновление не выполняется.',
        catalog),
] };
