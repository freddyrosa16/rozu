from functions.get_files_info import get_files_info
print(get_files_info("calculator", "."))
print(get_files_info("calculator", "/bin"))
print(get_files_info("calculator", "../"))
print(get_files_info("calculator", "main.py"), '\n')

print('Result for current directory:')
print(get_files_info("calculator", "."), '\n')

print('Result for "pkg" directory:')
print(get_files_info("calculator", "pkg"), '\n')

print('Result for "/bin" directory:')
print(get_files_info("calculator", "/bin"), '\n')

print('Result for "../" directory:')
print(get_files_info("calculator", "../"), '\n')
